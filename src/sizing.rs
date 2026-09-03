//! Percent-of-equity position sizing for the cross-sectional momentum book.
//!
//! Each rebalance the strategy has a **gross budget** — `RISK_PCT` times the
//! account equity at that moment. That budget is split between the long and the
//! short side by `LONG_W` (0.5 = dollar-neutral), and each side's budget is
//! then spread across its legs by [`allocate`].
//!
//! With `tilt == 0.0` every leg on a side gets equal dollars. With `tilt > 0.0`
//! the allocation leans toward the higher-conviction names: the biggest winner
//! in the long decile and the biggest loser in the short decile.

use nautilus_model::identifiers::InstrumentId;

/// Split a gross `budget` into `(long_budget, short_budget)` by `long_w`.
/// `long_w` is clamped to `[0, 1]`.
pub fn split_sides(budget: f64, long_w: f64) -> (f64, f64) {
    let w = long_w.clamp(0.0, 1.0);
    (budget * w, budget * (1.0 - w))
}

/// Allocate `side_budget` USDT across one side's `legs`.
///
/// `legs` is `(instrument, signal)` for every name in that decile, where
/// `signal` is the raw trailing return used to rank the universe. `strongest`
/// picks which end of the signal is high-conviction: [`Conviction::High`] for
/// the long decile (largest return), [`Conviction::Low`] for the short decile
/// (most negative return).
///
/// `tilt >= 0.0` is the allocation lean. Each leg's weight is
/// `1 + tilt * s`, where `s ∈ [-1, 1]` is the leg's conviction rank within the
/// side (`+1` = strongest). Weights are floored at zero and renormalised so the
/// returned notionals sum to `side_budget`.
///
/// Returns `(instrument, notional_usdt)` in the input order. An empty or
/// non-positive `side_budget`, or empty `legs`, yields an empty vec.
pub fn allocate(
    side_budget: f64,
    legs: &[(InstrumentId, f64)],
    tilt: f64,
    strongest: Conviction,
) -> Vec<(InstrumentId, f64)> {
    if legs.is_empty() || !side_budget.is_finite() || side_budget <= 0.0 {
        return Vec::new();
    }

    let ranks = conviction_ranks(legs.iter().map(|&(_, s)| s), strongest);
    let weights: Vec<f64> = ranks
        .iter()
        .map(|&s| (1.0 + tilt.max(0.0) * s).max(0.0))
        .collect();

    let total: f64 = weights.iter().sum();
    let weights: Vec<f64> = if total > 0.0 {
        weights.into_iter().map(|w| w / total).collect()
    } else {
        // Degenerate (e.g. a single leg with rank -1 and tilt 1): equal-weight.
        let each = 1.0 / legs.len() as f64;
        vec![each; legs.len()]
    };

    legs.iter()
        .zip(weights)
        .map(|(&(id, _), w)| (id, side_budget * w))
        .collect()
}

/// Which end of the signal distribution carries the strongest conviction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Conviction {
    /// Largest signal value = strongest (the long decile).
    High,
    /// Smallest (most negative) signal value = strongest (the short decile).
    Low,
}

/// Map each signal to its rank within the slice, linearly spaced on `[-1, 1]`
/// with `+1` on the strongest-conviction end. Ties break by input order; a
/// single element ranks `+1`.
fn conviction_ranks(signals: impl Iterator<Item = f64>, strongest: Conviction) -> Vec<f64> {
    let signals: Vec<f64> = signals.collect();
    let n = signals.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![1.0];
    }

    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        signals[a]
            .partial_cmp(&signals[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut ranks = vec![0.0; n];
    for (position, &i) in order.iter().enumerate() {
        // position 0 = smallest signal -> -1.0; position n-1 = largest -> +1.0
        let ascending = -1.0 + 2.0 * position as f64 / (n - 1) as f64;
        ranks[i] = match strongest {
            Conviction::High => ascending,
            Conviction::Low => -ascending,
        };
    }
    ranks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<InstrumentId> {
        ["BTC", "ETH", "SOL", "XRP", "ADA", "DOGE"][..n]
            .iter()
            .map(|b| InstrumentId::from(format!("{b}USDT-LINEAR.BYBIT").as_str()))
            .collect()
    }

    fn notionals(v: &[(InstrumentId, f64)]) -> Vec<f64> {
        v.iter().map(|&(_, n)| n).collect()
    }

    #[test]
    fn split_sides_skews_and_clamps() {
        assert_eq!(split_sides(100.0, 0.5), (50.0, 50.0));
        let (l, s) = split_sides(100.0, 0.7);
        assert!((l - 70.0).abs() < 1e-9 && (s - 30.0).abs() < 1e-9);
        assert_eq!(split_sides(100.0, 2.0), (100.0, 0.0));
        assert_eq!(split_sides(100.0, -1.0), (0.0, 100.0));
    }

    #[test]
    fn zero_tilt_is_equal_dollars() {
        let id = ids(4);
        let legs = vec![
            (id[0], 0.5),
            (id[1], -0.1),
            (id[2], 0.2),
            (id[3], 0.9),
        ];
        let got = allocate(100.0, &legs, 0.0, Conviction::High);
        for (_, n) in &got {
            assert!((n - 25.0).abs() < 1e-9, "equal split, got {n}");
        }
        assert!((notionals(&got).iter().sum::<f64>() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn tilt_favours_the_strongest_long_name() {
        let id = ids(3);
        // signals ascending: ETH < BTC < SOL  => SOL is the strongest long
        let legs = vec![(id[0], 0.1), (id[1], 0.2), (id[2], 0.3)];
        let got = allocate(90.0, &legs, 1.0, Conviction::High);
        let n = notionals(&got);
        assert!(n[2] > n[1] && n[1] > n[0], "monotone in rank: {n:?}");
        assert!((n.iter().sum::<f64>() - 90.0).abs() < 1e-9);
        // ranks -1, 0, +1 -> weights 0, 1, 2 -> 0, 30, 60
        assert!((n[0] - 0.0).abs() < 1e-9);
        assert!((n[1] - 30.0).abs() < 1e-9);
        assert!((n[2] - 60.0).abs() < 1e-9);
    }

    #[test]
    fn tilt_favours_the_most_negative_short_name() {
        let id = ids(3);
        // most negative return is the strongest short
        let legs = vec![(id[0], -0.30), (id[1], -0.20), (id[2], -0.05)];
        let got = allocate(90.0, &legs, 1.0, Conviction::Low);
        let n = notionals(&got);
        assert!(n[0] > n[1] && n[1] > n[2], "most negative gets the most: {n:?}");
        assert!((n.iter().sum::<f64>() - 90.0).abs() < 1e-9);
    }

    #[test]
    fn empty_or_zero_budget_allocates_nothing() {
        let id = ids(2);
        let legs = vec![(id[0], 0.1), (id[1], 0.2)];
        assert!(allocate(0.0, &legs, 0.0, Conviction::High).is_empty());
        assert!(allocate(-5.0, &legs, 0.0, Conviction::High).is_empty());
        assert!(allocate(100.0, &[], 0.0, Conviction::High).is_empty());
    }

    #[test]
    fn single_leg_takes_the_whole_side() {
        let id = ids(1);
        let got = allocate(42.0, &[(id[0], 0.1)], 3.0, Conviction::Low);
        assert_eq!(got.len(), 1);
        assert!((got[0].1 - 42.0).abs() < 1e-9);
    }
}
