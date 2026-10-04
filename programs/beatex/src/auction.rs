//! Deterministic plaintext auction primitives.
//!
//! The production functions in this module operate on borrowed slices and do
//! not allocate. This keeps the clearing path predictable on-chain and makes
//! the same implementation usable by off-chain clients that independently
//! recompute a result.

/// Errors returned while validating or clearing plaintext auction data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuctionError {
    EmptyGrid,
    EvenGridSize,
    ZeroTickSize,
    PriceUnderflow,
    PriceOverflow,
    IndexOutOfBounds,
    CurveLengthMismatch,
    BothSidesAtLevel { index: u16 },
    DemandOverflow { index: u16 },
    SupplyOverflow { index: u16 },
}

/// An odd-sized price grid centered on the previous clearing price.
///
/// `reference_price` and `tick_size` are expressed in price lots. The center
/// level is `level_count / 2`, and is exactly `reference_price`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PriceGrid {
    pub reference_price: u64,
    pub tick_size: u64,
    pub level_count: u16,
}

impl PriceGrid {
    /// Validate the grid shape and every price implied by its two edges.
    pub fn validate(&self) -> Result<(), AuctionError> {
        validate_odd_level_count(self.level_count)?;
        if self.tick_size == 0 {
            return Err(AuctionError::ZeroTickSize);
        }

        let radius = u64::from(self.center_index());
        let edge_delta = radius
            .checked_mul(self.tick_size)
            .ok_or(AuctionError::PriceOverflow)?;
        self.reference_price
            .checked_sub(edge_delta)
            .ok_or(AuctionError::PriceUnderflow)?;
        self.reference_price
            .checked_add(edge_delta)
            .ok_or(AuctionError::PriceOverflow)?;
        Ok(())
    }

    /// Index of the reference price.
    pub const fn center_index(&self) -> u16 {
        self.level_count / 2
    }

    /// Return a price in price lots for a validated grid index.
    pub fn price_at(&self, index: u16) -> Result<u64, AuctionError> {
        self.validate()?;
        if index >= self.level_count {
            return Err(AuctionError::IndexOutOfBounds);
        }

        let center = self.center_index();
        if index <= center {
            let delta = u64::from(center - index)
                .checked_mul(self.tick_size)
                .ok_or(AuctionError::PriceOverflow)?;
            self.reference_price
                .checked_sub(delta)
                .ok_or(AuctionError::PriceUnderflow)
        } else {
            let delta = u64::from(index - center)
                .checked_mul(self.tick_size)
                .ok_or(AuctionError::PriceOverflow)?;
            self.reference_price
                .checked_add(delta)
                .ok_or(AuctionError::PriceOverflow)
        }
    }

    /// Iterate in clearing tie-break order: `k, k-1, k+1, ...`.
    pub fn zigzag_indices(&self) -> Result<ZigZagIndices, AuctionError> {
        self.validate()?;
        Ok(ZigZagIndices::new(self.level_count))
    }
}

/// Allocation-free iterator over an odd grid in clearing tie-break order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZigZagIndices {
    level_count: u16,
    center: u16,
    position: u16,
}

impl ZigZagIndices {
    const fn new(level_count: u16) -> Self {
        Self {
            level_count,
            center: level_count / 2,
            position: 0,
        }
    }
}

impl Iterator for ZigZagIndices {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        if self.position >= self.level_count {
            return None;
        }

        let position = self.position;
        self.position += 1;

        if position == 0 {
            return Some(self.center);
        }

        // Odd positions visit the lower index first; even positions then visit
        // the equally distant upper index.
        let distance = position.div_ceil(2);
        if position % 2 == 1 {
            Some(self.center - distance)
        } else {
            Some(self.center + distance)
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = usize::from(self.level_count - self.position);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for ZigZagIndices {}

/// One plaintext order curve over a market's fixed price grid.
///
/// Buy and sell entries are independent quantities of the base asset that are
/// contingent on that exact index becoming the clearing price. At most one
/// side may be non-zero at a given level; no monotonicity is required.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderCurve<'a> {
    pub buy: &'a [u64],
    pub sell: &'a [u64],
}

impl OrderCurve<'_> {
    pub fn validate(&self, grid: &PriceGrid) -> Result<(), AuctionError> {
        grid.validate()?;
        validate_curve_lengths(grid.level_count, self.buy, self.sell)?;
        validate_curve_shape(self.buy, self.sell)
    }
}

/// Validate decrypted aggregate demand and supply curves.
///
/// Side exclusivity is deliberately not required for aggregates: independent
/// orders can contribute liquidity to opposite sides at the same price.
pub fn validate_aggregate_curves(
    grid: &PriceGrid,
    demand: &[u64],
    supply: &[u64],
) -> Result<(), AuctionError> {
    grid.validate()?;
    validate_curve_lengths(grid.level_count, demand, supply)?;
    Ok(())
}

/// Add one validated order to caller-owned aggregate curves.
///
/// Overflow and validity are checked before either aggregate is mutated. An
/// error therefore leaves both destination slices unchanged.
pub fn aggregate_order(
    grid: &PriceGrid,
    aggregate_demand: &mut [u64],
    aggregate_supply: &mut [u64],
    order: OrderCurve<'_>,
) -> Result<(), AuctionError> {
    order.validate(grid)?;
    validate_aggregate_curves(grid, aggregate_demand, aggregate_supply)?;

    for index in 0..usize::from(grid.level_count) {
        aggregate_demand[index]
            .checked_add(order.buy[index])
            .ok_or(AuctionError::DemandOverflow {
                index: index as u16,
            })?;
        aggregate_supply[index]
            .checked_add(order.sell[index])
            .ok_or(AuctionError::SupplyOverflow {
                index: index as u16,
            })?;
    }

    for index in 0..usize::from(grid.level_count) {
        // Safe because the complete overflow preflight above succeeded.
        aggregate_demand[index] += order.buy[index];
        aggregate_supply[index] += order.sell[index];
    }

    Ok(())
}

/// Immutable result of deterministic maximum-volume clearing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClearingResult {
    pub index: u16,
    pub price: u64,
    pub executable_volume: u64,
    pub aggregate_demand: u64,
    pub aggregate_supply: u64,
}

/// Select the maximum executable-volume price using the exact zigzag tie-break.
///
/// Equal volumes never replace the current winner. Since candidates are
/// visited `k, k-1, k+1, ...`, this selects the price closest to the previous
/// clearing price and the lower index when two candidates are equally distant.
pub fn clear_auction(
    grid: &PriceGrid,
    aggregate_demand: &[u64],
    aggregate_supply: &[u64],
) -> Result<ClearingResult, AuctionError> {
    validate_aggregate_curves(grid, aggregate_demand, aggregate_supply)?;

    let mut indices = grid.zigzag_indices()?;
    let center = indices.next().expect("a validated odd grid is non-empty");
    let center_index = usize::from(center);
    let mut best_index = center;
    let mut best_volume = aggregate_demand[center_index].min(aggregate_supply[center_index]);

    for index in indices {
        let slice_index = usize::from(index);
        let volume = aggregate_demand[slice_index].min(aggregate_supply[slice_index]);
        if volume > best_volume {
            best_index = index;
            best_volume = volume;
        }
    }

    let slice_index = usize::from(best_index);
    Ok(ClearingResult {
        index: best_index,
        price: grid.price_at(best_index)?,
        executable_volume: best_volume,
        aggregate_demand: aggregate_demand[slice_index],
        aggregate_supply: aggregate_supply[slice_index],
    })
}

fn validate_odd_level_count(level_count: u16) -> Result<(), AuctionError> {
    if level_count == 0 {
        return Err(AuctionError::EmptyGrid);
    }
    if level_count % 2 == 0 {
        return Err(AuctionError::EvenGridSize);
    }
    Ok(())
}

fn validate_curve_lengths(
    level_count: u16,
    first: &[u64],
    second: &[u64],
) -> Result<(), AuctionError> {
    let expected = usize::from(level_count);
    if first.len() != expected || second.len() != expected {
        return Err(AuctionError::CurveLengthMismatch);
    }
    Ok(())
}

fn validate_curve_shape(buy: &[u64], sell: &[u64]) -> Result<(), AuctionError> {
    for (index, (&buy_quantity, &sell_quantity)) in buy.iter().zip(sell).enumerate() {
        if buy_quantity != 0 && sell_quantity != 0 {
            return Err(AuctionError::BothSidesAtLevel {
                index: index as u16,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid_5() -> PriceGrid {
        PriceGrid {
            reference_price: 100,
            tick_size: 10,
            level_count: 5,
        }
    }

    #[test]
    fn validates_grid_and_computes_every_price() {
        let grid = grid_5();
        assert_eq!(grid.validate(), Ok(()));
        assert_eq!(grid.center_index(), 2);
        assert_eq!(
            (0..grid.level_count)
                .map(|index| grid.price_at(index).unwrap())
                .collect::<Vec<_>>(),
            vec![80, 90, 100, 110, 120]
        );
    }

    #[test]
    fn a_single_level_grid_is_valid_and_centered() {
        let grid = PriceGrid {
            reference_price: 7,
            tick_size: 1,
            level_count: 1,
        };
        assert_eq!(grid.validate(), Ok(()));
        assert_eq!(grid.price_at(0), Ok(7));
        assert_eq!(grid.zigzag_indices().unwrap().collect::<Vec<_>>(), vec![0]);
    }

    #[test]
    fn rejects_invalid_grid_shapes_and_edges() {
        assert_eq!(
            PriceGrid {
                reference_price: 1,
                tick_size: 1,
                level_count: 0,
            }
            .validate(),
            Err(AuctionError::EmptyGrid)
        );
        assert_eq!(
            PriceGrid {
                reference_price: 1,
                tick_size: 1,
                level_count: 2,
            }
            .validate(),
            Err(AuctionError::EvenGridSize)
        );
        assert_eq!(
            PriceGrid {
                reference_price: 1,
                tick_size: 0,
                level_count: 1,
            }
            .validate(),
            Err(AuctionError::ZeroTickSize)
        );
        assert_eq!(
            PriceGrid {
                reference_price: 9,
                tick_size: 10,
                level_count: 3,
            }
            .validate(),
            Err(AuctionError::PriceUnderflow)
        );
        assert_eq!(
            PriceGrid {
                reference_price: u64::MAX - 9,
                tick_size: 10,
                level_count: 3,
            }
            .validate(),
            Err(AuctionError::PriceOverflow)
        );
        assert_eq!(
            PriceGrid {
                reference_price: u64::MAX,
                tick_size: u64::MAX,
                level_count: 5,
            }
            .validate(),
            Err(AuctionError::PriceOverflow)
        );
    }

    #[test]
    fn rejects_an_out_of_grid_index() {
        assert_eq!(grid_5().price_at(5), Err(AuctionError::IndexOutOfBounds));
    }

    #[test]
    fn zigzag_order_is_exact_for_multiple_grid_sizes() {
        let make_grid = |level_count| PriceGrid {
            reference_price: 1_000,
            tick_size: 1,
            level_count,
        };

        assert_eq!(
            make_grid(3).zigzag_indices().unwrap().collect::<Vec<_>>(),
            vec![1, 0, 2]
        );
        assert_eq!(
            make_grid(5).zigzag_indices().unwrap().collect::<Vec<_>>(),
            vec![2, 1, 3, 0, 4]
        );
        assert_eq!(
            make_grid(9).zigzag_indices().unwrap().collect::<Vec<_>>(),
            vec![4, 3, 5, 2, 6, 1, 7, 0, 8]
        );
    }

    #[test]
    fn validates_a_two_sided_order_with_a_spread() {
        let order = OrderCurve {
            buy: &[9, 7, 0, 0, 0],
            sell: &[0, 0, 0, 3, 8],
        };
        assert_eq!(order.validate(&grid_5()), Ok(()));
    }

    #[test]
    fn permits_zero_and_one_sided_orders() {
        assert_eq!(
            OrderCurve {
                buy: &[0, 0, 0, 0, 0],
                sell: &[0, 0, 0, 0, 0],
            }
            .validate(&grid_5()),
            Ok(())
        );
        assert_eq!(
            OrderCurve {
                buy: &[5, 5, 2, 0, 0],
                sell: &[0, 0, 0, 0, 0],
            }
            .validate(&grid_5()),
            Ok(())
        );
    }

    #[test]
    fn rejects_order_length_mismatch() {
        let order = OrderCurve {
            buy: &[1, 0],
            sell: &[0, 0],
        };
        assert_eq!(
            order.validate(&grid_5()),
            Err(AuctionError::CurveLengthMismatch)
        );
    }

    #[test]
    fn rejects_both_sides_at_the_same_level() {
        let order = OrderCurve {
            buy: &[5, 4, 3, 0, 0],
            sell: &[0, 0, 1, 2, 3],
        };
        assert_eq!(
            order.validate(&grid_5()),
            Err(AuctionError::BothSidesAtLevel { index: 2 })
        );
    }

    #[test]
    fn accepts_non_monotonic_contingent_schedules() {
        assert_eq!(
            OrderCurve {
                buy: &[5, 6, 0, 0, 0],
                sell: &[0, 0, 0, 1, 2],
            }
            .validate(&grid_5()),
            Ok(())
        );
        assert_eq!(
            OrderCurve {
                buy: &[5, 4, 0, 0, 0],
                sell: &[0, 0, 0, 2, 1],
            }
            .validate(&grid_5()),
            Ok(())
        );
    }

    #[test]
    fn aggregates_orders_element_by_element() {
        let grid = grid_5();
        let mut demand = [0; 5];
        let mut supply = [0; 5];

        aggregate_order(
            &grid,
            &mut demand,
            &mut supply,
            OrderCurve {
                buy: &[8, 6, 0, 0, 0],
                sell: &[0, 0, 0, 4, 7],
            },
        )
        .unwrap();
        aggregate_order(
            &grid,
            &mut demand,
            &mut supply,
            OrderCurve {
                buy: &[3, 2, 1, 0, 0],
                sell: &[0, 0, 0, 2, 2],
            },
        )
        .unwrap();

        assert_eq!(demand, [11, 8, 1, 0, 0]);
        assert_eq!(supply, [0, 0, 0, 6, 9]);
        assert_eq!(validate_aggregate_curves(&grid, &demand, &supply), Ok(()));
    }

    #[test]
    fn aggregation_overflow_is_atomic() {
        let grid = grid_5();
        let mut demand = [u64::MAX, 4, 3, 2, 1];
        let mut supply = [0, 0, 0, 0, 0];
        let original_demand = demand;
        let original_supply = supply;

        let result = aggregate_order(
            &grid,
            &mut demand,
            &mut supply,
            OrderCurve {
                buy: &[1, 0, 0, 0, 0],
                sell: &[0, 0, 0, 0, 0],
            },
        );

        assert_eq!(result, Err(AuctionError::DemandOverflow { index: 0 }));
        assert_eq!(demand, original_demand);
        assert_eq!(supply, original_supply);
    }

    #[test]
    fn invalid_order_does_not_mutate_aggregates() {
        let grid = grid_5();
        let mut demand = [5, 4, 3, 2, 1];
        let mut supply = [1, 2, 3, 4, 5];
        let original_demand = demand;
        let original_supply = supply;

        let result = aggregate_order(
            &grid,
            &mut demand,
            &mut supply,
            OrderCurve {
                buy: &[2, 1, 1, 0, 0],
                sell: &[0, 0, 1, 2, 2],
            },
        );

        assert_eq!(result, Err(AuctionError::BothSidesAtLevel { index: 2 }));
        assert_eq!(demand, original_demand);
        assert_eq!(supply, original_supply);
    }

    #[test]
    fn arbitrary_existing_aggregate_shapes_are_accepted() {
        let grid = grid_5();
        let mut demand = [5, 4, 6, 2, 1];
        let mut supply = [0, 1, 2, 3, 4];

        let result = aggregate_order(
            &grid,
            &mut demand,
            &mut supply,
            OrderCurve {
                buy: &[1, 1, 0, 0, 0],
                sell: &[0, 0, 0, 1, 1],
            },
        );

        assert_eq!(result, Ok(()));
        assert_eq!(demand, [6, 5, 6, 2, 1]);
        assert_eq!(supply, [0, 1, 2, 4, 5]);
    }

    #[test]
    fn accepts_arbitrary_decrypted_aggregate_schedules() {
        assert_eq!(
            validate_aggregate_curves(&grid_5(), &[5, 4, 6, 2, 1], &[1, 2, 3, 4, 5]),
            Ok(())
        );
        assert_eq!(
            validate_aggregate_curves(&grid_5(), &[5, 4, 3, 2, 1], &[1, 3, 2, 4, 5]),
            Ok(())
        );
    }

    #[test]
    fn clears_at_the_unique_maximum_volume() {
        let result = clear_auction(&grid_5(), &[12, 10, 8, 5, 1], &[1, 4, 7, 9, 12]).unwrap();

        assert_eq!(
            result,
            ClearingResult {
                index: 2,
                price: 100,
                executable_volume: 7,
                aggregate_demand: 8,
                aggregate_supply: 7,
            }
        );
    }

    #[test]
    fn closest_price_wins_a_maximum_volume_plateau() {
        // Executable volumes are [1, 4, 6, 6, 5]. Indices 2 and 3 tie,
        // and index 2 is the reference price visited first.
        let result = clear_auction(&grid_5(), &[10, 9, 8, 6, 5], &[1, 4, 6, 8, 9]).unwrap();

        assert_eq!(result.index, 2);
        assert_eq!(result.executable_volume, 6);
    }

    #[test]
    fn lower_index_is_visited_before_equally_distant_upper_index() {
        let order = grid_5().zigzag_indices().unwrap().collect::<Vec<_>>();
        assert!(
            order.iter().position(|&index| index == 1) < order.iter().position(|&index| index == 3)
        );
        assert!(
            order.iter().position(|&index| index == 0) < order.iter().position(|&index| index == 4)
        );
    }

    #[test]
    fn all_zero_volume_keeps_the_reference_price() {
        let result = clear_auction(&grid_5(), &[9, 7, 5, 3, 1], &[0, 0, 0, 0, 0]).unwrap();

        assert_eq!(result.index, 2);
        assert_eq!(result.price, 100);
        assert_eq!(result.executable_volume, 0);
        assert_eq!(result.aggregate_demand, 5);
        assert_eq!(result.aggregate_supply, 0);
    }

    #[test]
    fn clearing_rejects_wrong_lengths_and_bad_grid() {
        assert_eq!(
            clear_auction(&grid_5(), &[1, 0], &[0, 1]),
            Err(AuctionError::CurveLengthMismatch)
        );

        let even_grid = PriceGrid {
            reference_price: 100,
            tick_size: 1,
            level_count: 4,
        };
        assert_eq!(
            clear_auction(&even_grid, &[0; 4], &[0; 4]),
            Err(AuctionError::EvenGridSize)
        );
    }
}
