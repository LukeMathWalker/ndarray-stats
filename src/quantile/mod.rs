use self::interpolate::Interpolate;
use std::collections::BTreeSet;
use ndarray::prelude::*;
use ndarray::{Data, DataMut, RemoveAxis};
use std::cmp;
use super::sort::sorted_get_many_mut_unchecked;
use {MaybeNan, MaybeNanExt};

/// Quantile methods for `ArrayBase`.
pub trait QuantileExt<A, S, D>
where
    S: Data<Elem = A>,
    D: Dimension,
{
    /// Finds the elementwise minimum of the array.
    ///
    /// Returns `None` if any of the pairwise orderings tested by the function
    /// are undefined. (For example, this occurs if there are any
    /// floating-point NaN values in the array.)
    ///
    /// Additionally, returns `None` if the array is empty.
    fn min(&self) -> Option<&A>
    where
        A: PartialOrd;

    /// Finds the elementwise minimum of the array, skipping NaN values.
    ///
    /// **Warning** This method will return a NaN value if none of the values
    /// in the array are non-NaN values. Note that the NaN value might not be
    /// in the array.
    fn min_skipnan(&self) -> &A
    where
        A: MaybeNan,
        A::NotNan: Ord;

    /// Finds the elementwise maximum of the array.
    ///
    /// Returns `None` if any of the pairwise orderings tested by the function
    /// are undefined. (For example, this occurs if there are any
    /// floating-point NaN values in the array.)
    ///
    /// Additionally, returns `None` if the array is empty.
    fn max(&self) -> Option<&A>
    where
        A: PartialOrd;

    /// Finds the elementwise maximum of the array, skipping NaN values.
    ///
    /// **Warning** This method will return a NaN value if none of the values
    /// in the array are non-NaN values. Note that the NaN value might not be
    /// in the array.
    fn max_skipnan(&self) -> &A
    where
        A: MaybeNan,
        A::NotNan: Ord;

    /// Return the qth quantile of the data along the specified axis.
    ///
    /// `q` needs to be a float between 0 and 1, bounds included.
    /// The qth quantile for a 1-dimensional lane of length `N` is defined
    /// as the element that would be indexed as `(N-1)q` if the lane were to be sorted
    /// in increasing order.
    /// If `(N-1)q` is not an integer the desired quantile lies between
    /// two data points: we return the lower, nearest, higher or interpolated
    /// value depending on the type `Interpolate` bound `I`.
    ///
    /// Some examples:
    /// - `q=0.` returns the minimum along each 1-dimensional lane;
    /// - `q=0.5` returns the median along each 1-dimensional lane;
    /// - `q=1.` returns the maximum along each 1-dimensional lane.
    /// (`q=0` and `q=1` are considered improper quantiles)
    ///
    /// The array is shuffled **in place** along each 1-dimensional lane in
    /// order to produce the required quantile without allocating a copy
    /// of the original array. Each 1-dimensional lane is shuffled independently
    /// from the others.
    /// No assumptions should be made on the ordering of the array elements
    /// after this computation.
    ///
    /// Complexity ([quickselect](https://en.wikipedia.org/wiki/Quickselect)):
    /// - average case: O(`m`);
    /// - worst case: O(`m`^2);
    /// where `m` is the number of elements in the array.
    ///
    /// **Panics** if `axis` is out of bounds, if the axis has length 0, or if
    /// `q` is not between `0.` and `1.` (inclusive).
    fn quantile_axis_mut<I>(&mut self, axis: Axis, q: f64) -> Array<A, D::Smaller>
    where
        D: RemoveAxis,
        A: Ord + Clone,
        S: DataMut,
        I: Interpolate<A>;

    fn quantiles_axis_mut<I>(&mut self, axis: Axis, qs: &[f64]) -> Vec<Array<A, D::Smaller>>
        where
            D: RemoveAxis,
            A: Ord + Clone,
            S: DataMut,
            I: Interpolate<A>;

    /// Return the `q`th quantile of the data along the specified axis, skipping NaN values.
    ///
    /// See [`quantile_axis_mut`](##tymethod.quantile_axis_mut) for details.
    fn quantile_axis_skipnan_mut<I>(&mut self, axis: Axis, q: f64) -> Array<A, D::Smaller>
    where
        D: RemoveAxis,
        A: MaybeNan,
        A::NotNan: Clone + Ord,
        S: DataMut,
        I: Interpolate<A::NotNan>;
}

impl<A, S, D> QuantileExt<A, S, D> for ArrayBase<S, D>
where
    S: Data<Elem = A>,
    D: Dimension,
{
    fn min(&self) -> Option<&A>
    where
        A: PartialOrd,
    {
        let first = self.first()?;
        self.fold(Some(first), |acc, elem| match elem.partial_cmp(acc?)? {
            cmp::Ordering::Less => Some(elem),
            _ => acc,
        })
    }

    fn min_skipnan(&self) -> &A
    where
        A: MaybeNan,
        A::NotNan: Ord,
    {
        let first = self.first().and_then(|v| v.try_as_not_nan());
        A::from_not_nan_ref_opt(self.fold_skipnan(first, |acc, elem| {
            Some(match acc {
                Some(acc) => acc.min(elem),
                None => elem,
            })
        }))
    }

    fn max(&self) -> Option<&A>
    where
        A: PartialOrd,
    {
        let first = self.first()?;
        self.fold(Some(first), |acc, elem| match elem.partial_cmp(acc?)? {
            cmp::Ordering::Greater => Some(elem),
            _ => acc,
        })
    }

    fn max_skipnan(&self) -> &A
    where
        A: MaybeNan,
        A::NotNan: Ord,
    {
        let first = self.first().and_then(|v| v.try_as_not_nan());
        A::from_not_nan_ref_opt(self.fold_skipnan(first, |acc, elem| {
            Some(match acc {
                Some(acc) => acc.max(elem),
                None => elem,
            })
        }))
    }

    fn quantiles_axis_mut<I>(&mut self, axis: Axis, qs: &[f64]) -> Vec<Array<A, D::Smaller>>
        where
            D: RemoveAxis,
            A: Ord + Clone,
            S: DataMut,
            I: Interpolate<A>,
    {
        assert!(qs.iter().all(|x| (0. <= *x) && (*x <= 1.)));

        let mut deduped_qs: Vec<f64> = qs.to_vec();
        deduped_qs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        deduped_qs.dedup();

        let axis_len = self.len_of(axis);
        let mut searched_indexes = BTreeSet::new();
        for q in deduped_qs.iter() {
            if I::needs_lower(*q, axis_len) {
                searched_indexes.insert(I::lower_index(*q, axis_len));
            }
            if I::needs_higher(*q, axis_len) {
                searched_indexes.insert(I::higher_index(*q, axis_len));
            }
        }
        let searched_indexes: Vec<usize> = searched_indexes.into_iter().collect();

        let values = self.map_axis_mut(
                axis,
                |mut x| sorted_get_many_mut_unchecked(&mut x, &searched_indexes)
        );

        let mut results = vec![];
        for q in qs {
            let result = I::interpolate(
                match I::needs_lower(*q, axis_len) {
                    true => Some(
                        values.map(
                            |x| x.get(&I::lower_index(*q, axis_len)).unwrap().clone())
                    ),
                    false => None,
                },
                match I::needs_higher(*q, axis_len) {
                    true => Some(
                        values.map(
                            |x| x.get(&I::higher_index(*q, axis_len)).unwrap().clone())
                    ),
                    false => None,
                },
                *q,
                axis_len
            );
            results.push(result);
        }
        results
    }

    fn quantile_axis_mut<I>(&mut self, axis: Axis, q: f64) -> Array<A, D::Smaller>
    where
        D: RemoveAxis,
        A: Ord + Clone,
        S: DataMut,
        I: Interpolate<A>,
    {
        self.quantiles_axis_mut::<I>(axis, &[q]).into_iter().next().unwrap()
    }

    fn quantile_axis_skipnan_mut<I>(&mut self, axis: Axis, q: f64) -> Array<A, D::Smaller>
    where
        D: RemoveAxis,
        A: MaybeNan,
        A::NotNan: Clone + Ord,
        S: DataMut,
        I: Interpolate<A::NotNan>,
    {
        self.map_axis_mut(axis, |lane| {
            let mut not_nan = A::remove_nan_mut(lane);
            A::from_not_nan_opt(if not_nan.is_empty() {
                None
            } else {
                Some(
                    not_nan
                        .quantile_axis_mut::<I>(Axis(0), q)
                        .into_raw_vec()
                        .remove(0),
                )
            })
        })
    }
}

/// Quantile methods for 1-D arrays.
pub trait Quantile1dExt<A, S>
    where
        S: Data<Elem = A>,
{
    /// Return the qth quantile of the data.
    ///
    /// `q` needs to be a float between 0 and 1, bounds included.
    /// The qth quantile for a 1-dimensional array of length `N` is defined
    /// as the element that would be indexed as `(N-1)q` if the array were to be sorted
    /// in increasing order.
    /// If `(N-1)q` is not an integer the desired quantile lies between
    /// two data points: we return the lower, nearest, higher or interpolated
    /// value depending on the type `Interpolate` bound `I`.
    ///
    /// Some examples:
    /// - `q=0.` returns the minimum;
    /// - `q=0.5` returns the median;
    /// - `q=1.` returns the maximum.
    /// (`q=0` and `q=1` are considered improper quantiles)
    ///
    /// The array is shuffled **in place** in order to produce the required quantile
    /// without allocating a copy.
    /// No assumptions should be made on the ordering of the array elements
    /// after this computation.
    ///
    /// Complexity ([quickselect](https://en.wikipedia.org/wiki/Quickselect)):
    /// - average case: O(`m`);
    /// - worst case: O(`m`^2);
    /// where `m` is the number of elements in the array.
    ///
    /// Returns `None` if the array is empty.
    ///
    /// **Panics** if `q` is not between `0.` and `1.` (inclusive).
    fn quantile_mut<I>(&mut self, q: f64) -> Option<A>
    where
        A: Ord + Clone,
        S: DataMut,
        I: Interpolate<A>;
}

impl<A, S> Quantile1dExt<A, S> for ArrayBase<S, Ix1>
    where
        S: Data<Elem = A>,
{
    fn quantile_mut<I>(&mut self, q: f64) -> Option<A>
    where
        A: Ord + Clone,
        S: DataMut,
        I: Interpolate<A>,
    {
        if self.is_empty() {
            None
        } else {
            Some(self.quantile_axis_mut::<I>(Axis(0), q).into_scalar())
        }
    }
}

pub mod interpolate;

