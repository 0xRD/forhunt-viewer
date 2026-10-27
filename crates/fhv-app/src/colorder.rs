//! Column reordering by dragging headers (D-070): the pure part.

use std::collections::HashMap;

use crate::tab::DC_FIRST;

/// Moves display column `from` before display slot `slot` (a slot is the
/// gap before display column `slot`; `display_count` = after the last).
/// `into_frozen`: the drop landed in the frozen zone. A column that changes
/// zone moves the freeze boundary by one, so every other column keeps its
/// zone. Returns false when nothing changed.
pub fn move_column(
    order: &mut Vec<usize>,
    frozen: &mut usize,
    from: usize,
    slot: usize,
    into_frozen: bool,
) -> bool {
    if from < DC_FIRST || from - DC_FIRST >= order.len() {
        return false;
    }
    let i = from - DC_FIRST;
    let s = slot.saturating_sub(DC_FIRST).min(order.len());
    let to = if s > i { s - 1 } else { s };
    let was_frozen = from < *frozen;
    let mut changed = false;
    if to != i {
        let c = order.remove(i);
        order.insert(to, c);
        changed = true;
    }
    if was_frozen && !into_frozen {
        *frozen -= 1;
        changed = true;
    } else if !was_frozen && into_frozen {
        *frozen += 1;
        changed = true;
    }
    changed
}

/// Hides column `c`, remembering the visible column before it.
pub fn hide(order: &mut Vec<usize>, after: &mut HashMap<usize, Option<usize>>, c: usize) {
    if let Some(i) = order.iter().position(|&x| x == c) {
        after.insert(c, i.checked_sub(1).map(|p| order[p]));
        order.remove(i);
    }
}

/// Shows column `c` again, right after the column it followed when hidden
/// (at the end when that one is hidden too, or it was never shown).
pub fn show(order: &mut Vec<usize>, after: &mut HashMap<usize, Option<usize>>, c: usize) {
    if order.contains(&c) {
        return;
    }
    let at = match after.remove(&c) {
        Some(None) => 0,
        Some(Some(p)) => order
            .iter()
            .position(|&x| x == p)
            .map_or(order.len(), |i| i + 1),
        None => order.len(),
    };
    order.insert(at, c);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_and_keeps_zones() {
        // Display columns: 0 tag, 1 #, 2.. file columns.
        let mut o = vec![0, 1, 2, 3];
        let mut f = 0;
        assert!(move_column(&mut o, &mut f, 2, 5, false));
        assert_eq!(o, vec![1, 2, 0, 3]);
        assert!(move_column(&mut o, &mut f, 5, 2, false));
        assert_eq!(o, vec![3, 1, 2, 0]);
        assert!(!move_column(&mut o, &mut f, 2, 2, false));
        assert!(!move_column(&mut o, &mut f, 2, 3, false));
        assert!(!move_column(&mut o, &mut f, 0, 3, false));
        // Frozen: tag, #, first two file columns.
        let mut o = vec![0, 1, 2, 3];
        let mut f = 4;
        // Out of the frozen zone: the boundary shrinks.
        assert!(move_column(&mut o, &mut f, 2, 6, false));
        assert_eq!((o.clone(), f), (vec![1, 2, 3, 0], 3));
        // Into the frozen zone: the boundary grows.
        assert!(move_column(&mut o, &mut f, 5, 2, true));
        assert_eq!((o.clone(), f), (vec![0, 1, 2, 3], 4));
        // Same zone: the boundary stays.
        assert!(move_column(&mut o, &mut f, 2, 4, true));
        assert_eq!((o, f), (vec![1, 0, 2, 3], 4));
    }

    #[test]
    fn hidden_columns_return_to_their_place() {
        let mut o = vec![0, 1, 2, 3];
        let mut a = HashMap::new();
        hide(&mut o, &mut a, 2);
        hide(&mut o, &mut a, 0);
        assert_eq!(o, vec![1, 3]);
        show(&mut o, &mut a, 2);
        assert_eq!(o, vec![1, 2, 3]);
        show(&mut o, &mut a, 0);
        assert_eq!(o, vec![0, 1, 2, 3]);
    }
}
