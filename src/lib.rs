//#![deny(missing_docs)]
#![cfg_attr(test, deny(warnings))]
#![cfg_attr(feature = "bench", feature(test))]

//! A generic, n-dimensional quadtree for fast neighbor lookups on multiple axes.

extern crate ref_slice;
use std::{mem, slice};
use self::NTreeVariant::{Branch, Bucket};

#[cfg(test)]
mod test;

/// The required interface for Regions in this n-tree.
///
/// Regions must be able to split themselves, tell if they overlap
/// other regions, and tell if a point is contained within the region.
pub trait Region<P>: Clone {
    /// Does this region contain this point?
    fn contains(&self, & P) -> bool;

    /// Split this region, returning a Vec of sub-regions.
    ///
    /// Invariants:
    ///   - The sub-regions must NOT overlap.
    ///   - All points in self must be contained within one and only one sub-region.
    fn split(&self) -> Vec<Self>;

    /// Does this region overlap with this other region?
    fn overlaps(& self, other: &Self) -> bool;
}

/// A quadtree-like structure, but for arbitrary arity.
///
/// Regions can split themselves into arbitrary numbers of splits,
/// allowing this structure to be used to index data by any number
/// of attributes and quickly query for data that falls within a
/// specific range.
pub struct NTree<R, P> {
    pub region: R,
    pub kind: NTreeVariant<R, P>
}

pub enum NTreeVariant<R, P> {
    /// A leaf of the tree, which contains points.
    Bucket {
        points: Vec<P>,
        bucket_limit: u8
    },
    /// An interior node of the tree, which contains n subtrees.
    Branch {
        subregions: Vec<NTree<R, P>>
    }
}

impl<P, R: Region<P>> NTree<R, P> {
    /// Create a new n-tree which contains points within
    /// the region and whose buckets are limited to the passed-in size.
    ///
    /// The number of regions returned by region.split() dictates
    /// the arity of the tree.
    pub fn new(region: R, size: u8) -> NTree<R, P> {
        NTree {
            kind: Branch {
                subregions: region
                    .split()
                    .into_iter()
                    .map(|r| NTree {
                        region: r,
                        kind: Bucket { points: vec![], bucket_limit: size }
                    })
                    .collect(),
            },
            region: region
        }
    }
    /// Insert a point into the n-tree, returns true if the point
    /// is within the n-tree and was inserted and false if not.
    pub fn insert(&mut self,  point: P) -> bool {
        if !self.region.contains(& point) { return false }

        match self.kind {
            Bucket { ref mut points, ref bucket_limit } => {
                if points.len() as u8 != *bucket_limit {
                    points.push(point);
                    return true
                }
            },
            Branch { ref mut subregions } => {
                match subregions.iter_mut().find(|r| r.contains(&point)) {
                    Some(ref mut subregion) => return subregion.insert(point),
                    None => return false
                }
            }
        };

        // Bucket is full
        split_and_insert(self, point);
        true
    }
    pub fn range_query<'t>(&'t self, query: R) -> RangeQuery<'t, R, P> {
        RangeQuery {
            query: query,
            points: (&[]).iter(),
            stack: vec![ref_slice::ref_slice(self).iter()],
        }
    }
    /// Get all the points which within the queried region.
    ///
    /// Finds all points which are located in regions overlapping
    /// the passed in region, then filters out all points which
    /// are not strictly within the region.
    pub fn range_query_mut<'t>(&'t mut self, query: R) -> RangeQueryMut<'t, R, P> {
        RangeQueryMut {
            stack: vec![ref_slice::mut_ref_slice(self).iter_mut()], //1 len slice w/ head of tree

            query: query,
            points: (&mut []).iter_mut(), //empty slice

        }
    }
    /// Is the point contained in the n-tree?
    pub fn contains(&self, point: & P) -> bool {
        self.region.contains(point)
    }

    /// Get all the points nearby a specified point.
    ///
    /// This will return no more than bucket_limit points.
    pub fn nearby<'a>(&'a self, point: & P) -> Option<&'a[P]> {
        if self.region.contains(point) {
            match self.kind {
                Bucket { ref points, .. } => Some(points.as_slice()),
                Branch { ref subregions } => {
                    subregions
                        .iter()
                        .find(|r| r.contains(point))
                        .and_then(|r| r.nearby(point))
                }
            }
        } else {
            None
        }
    }
    pub fn nearby_mut<'a>(&'a mut self, point: &mut P) -> Option<&'a mut[P]> {
        if self.region.contains(point) {
            match self.kind {
                Bucket { ref mut points, .. } => Some(&mut points[..]),
                Branch { ref mut subregions } => {
                    subregions
                        .iter_mut()
                        .find(|r| r.contains(point))
                        .and_then(|r| r.nearby_mut(point))
                }
            }
        } else {
            None
        }
    }

}

fn split_and_insert<P, R: Region<P>>(bucket: &mut NTree<R, P>, point: P) {
    let old_points;
    let old_bucket_limit;

    match bucket.kind {
        // Get the old region, points, and bucket limit.
        Bucket { ref mut points, bucket_limit } => {
            old_points = mem::replace(points, vec![]);
            old_bucket_limit = bucket_limit;
        },
        Branch { .. } => unreachable!()
    }

    // Replace the bucket with a split branch.
    *bucket = NTree::new(bucket.region.clone(), old_bucket_limit);

    // Insert all the old points into the right place.
    for old_point in old_points.into_iter() {
        bucket.insert(old_point);
    }

    // Finally, insert the new point.
    bucket.insert(point);
}


pub struct RangeQuery<'t, R: 't, P: 't> {
    pub query: R,
    pub points: slice::Iter<'t, P>,
    pub stack: Vec<slice::Iter<'t, NTree<R, P>>>
}


impl<'t, R: Region<P>, P> Iterator for RangeQuery<'t, R, P> {
    type Item = &'t P;

    fn next(&mut self) -> Option<&'t P> {
        'outer: loop {
            // try to find the next point in the region we're
            // currently examining.
            for p in &mut self.points {
                if self.query.contains(p) {
                    return Some(p)
                }
            }

            // no relevant points, so lets find a new region.

            'region_search: loop {
                let mut children_iter = match self.stack.pop() {
                    Some(x) => x,

                    // no more regions, so we're over.
                    None => return None,
                };

                'children: loop {
                    // look at the next item in the current sequence
                    // of children.
                    match children_iter.next() {
                        // this region is empty, next region!
                        None => continue 'region_search,

                        Some(value) => {
                            if value.region.overlaps(&self.query) {
                                // we always need to save this state, either we
                                // recur into a new region, or we break out and
                                // handle the points; either way, this is the
                                // last we touch `children_iter` for a little
                                // while.
                                self.stack.push(children_iter);

                                match value.kind {
                                    Bucket { ref points, .. } => {
                                        // found something with points
                                        self.points = points.iter();
                                        continue 'outer;
                                    }
                                    // step down into nested regions.
                                    Branch { ref subregions } => children_iter = subregions.iter()
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
/// An iterator over the points within a region.

// This iterates over the leaves of the tree from left-to-right by
// maintaining (a) the sequence of points at the current level
// (possibly empty), and (b) stack of iterators over the remaining
// children of the parents of the current point.
pub struct RangeQueryMut<'t, R: 't, P: 't> {
    query: R,
    points: slice::IterMut<'t, P>,
    stack: Vec<slice::IterMut<'t, NTree<R, P>>>,
       
}

impl<'t, R: Region<P>, P> Iterator for RangeQueryMut<'t, R, P> {
    type Item = &'t mut P;

    fn next(&mut self) -> Option<&'t mut P> {
        'outer: loop {
            for p in &mut self.points {
                if self.query.contains(p) {
                    return Some(p)
                }
            }
            'region_search: loop {
                let mut children_iter = match self.stack.pop() {
                    Some(x) => x,
                    None => return None,
                };
                'children: loop {
                    match children_iter.next() {
                        None => continue 'region_search,
                        Some(value) => {
                            if value.region.overlaps(&self.query) {
                                self.stack.push(children_iter);
                                match value.kind {
                                    Bucket { ref mut points, .. } => {
                                        self.points = points.iter_mut();
                                        continue 'outer;
                                    }
                                    Branch { ref  mut subregions } => children_iter = subregions.iter_mut()
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}




