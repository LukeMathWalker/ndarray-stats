use self::interpolate::Interpolate;
use std::collections::HashMap;
use ndarray::prelude::*;
use ndarray::{Data, DataMut, RemoveAxis, Slice};
use std::cmp;
use {MaybeNan, MaybeNanExt, Sort1dExt};

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

    fn quantile_axis_mut<I>(&mut self, axis: Axis, q: f64) -> Array<A, D::Smaller>
    where
        D: RemoveAxis,
        A: Ord + Clone,
        S: DataMut,
        I: Interpolate<A>,
    {
        assert!((0. <= q) && (q <= 1.));
        let axis_len = self.len_of(axis);
        let mut searched_indexes: Vec<usize> = vec![];
        if I::needs_lower(q, axis_len) {
            searched_indexes.push(I::lower_index(q, axis_len));
        }
        if I::needs_higher(q, axis_len) {
            searched_indexes.push(I::higher_index(q, axis_len));
        }

        let mut quantiles = HashMap::new();
        let mut previous_index = 0;
        let mut search_space = self.view_mut();
        for index in searched_indexes.into_iter() {
            let relative_index = index - previous_index;
            let quantile = Some(
                search_space.map_axis_mut(
                    axis, 
                    |mut x| x.sorted_get_mut(relative_index))
            );
            quantiles.insert(index, quantile);
            previous_index = index;
            search_space.slice_axis_inplace(axis, Slice::from(relative_index..));
        }

        I::interpolate(
            match I::needs_lower(q, axis_len) {
                true => quantiles.get(&I::lower_index(q, axis_len)).unwrap().clone(),
                false => None,
            },
            match I::needs_higher(q, axis_len) {
                true => quantiles.get(&I::higher_index(q, axis_len)).unwrap().clone(),
                false => None,
            },
            q,
            axis_len
        )
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

