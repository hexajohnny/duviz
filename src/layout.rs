use ratatui::layout::Rect;

pub struct BlockRect {
    pub index: usize,
    pub rect: Rect,
}

pub fn treemap(sizes: &[(usize, u64)], area: Rect) -> Vec<BlockRect> {
    if sizes.is_empty() || area.width == 0 || area.height == 0 {
        return Vec::new();
    }

    let total: u64 = sizes.iter().map(|(_, s)| *s).sum();
    let area_f = (area.width as f64) * (area.height as f64);

    let mut items: Vec<(usize, f64)> = sizes
        .iter()
        .map(|(idx, s)| {
            let v = if total == 0 { 1.0 } else { (*s as f64).max(1.0) };
            (*idx, v)
        })
        .collect();

    items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let total_f: f64 = items.iter().map(|(_, v)| *v).sum();
    let normalized: Vec<(usize, f64)> = items
        .into_iter()
        .map(|(idx, v)| (idx, v / total_f * area_f))
        .collect();

    let mut result = Vec::new();
    let mut rect = area;
    let mut row: Vec<(usize, f64)> = Vec::new();
    let mut row_min = f64::MAX;
    let mut row_max = 0.0;
    let mut row_sum = 0.0;

    let mut i = 0usize;
    while i < normalized.len() {
        let next = normalized[i];
        i += 1;

        if row.is_empty() {
            row.push(next);
            row_min = next.1;
            row_max = next.1;
            row_sum = next.1;
            continue;
        }

        let short = rect.width.min(rect.height) as f64;
        let worst_before = worst_ratio_stats(row_min, row_max, row_sum, short);
        let next_min = row_min.min(next.1);
        let next_max = row_max.max(next.1);
        let next_sum = row_sum + next.1;
        let worst_after = worst_ratio_stats(next_min, next_max, next_sum, short);

        if worst_after <= worst_before {
            row.push(next);
            row_min = next_min;
            row_max = next_max;
            row_sum = next_sum;
        } else {
            let (laid, new_rect) = layout_row(&row, rect, i >= normalized.len());
            result.extend(laid);
            rect = new_rect;
            row.clear();
            row.push(next);
            row_min = next.1;
            row_max = next.1;
            row_sum = next.1;
        }
    }

    if !row.is_empty() {
        let (laid, _new_rect) = layout_row(&row, rect, true);
        result.extend(laid);
    }

    result
}

pub fn grid_layout(sizes: &[(usize, u64)], area: Rect) -> Vec<BlockRect> {
    if sizes.is_empty() || area.width == 0 || area.height == 0 {
        return Vec::new();
    }

    let total: u64 = sizes.iter().map(|(_, s)| *s).sum();
    let total_f = if total == 0 { sizes.len() as f64 } else { total as f64 };

    let mut items: Vec<(usize, f64)> = sizes
        .iter()
        .map(|(idx, s)| {
            let v = if total == 0 { 1.0 } else { (*s as f64).max(1.0) };
            (*idx, v)
        })
        .collect();

    items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let n = items.len();
    let mut rows = (f64::from(n as u32).sqrt().ceil() as u16).max(1);
    if rows > area.height {
        rows = area.height.max(1);
    }
    let mut rows_vec: Vec<Vec<(usize, f64)>> = vec![Vec::new(); rows as usize];
    for (i, item) in items.into_iter().enumerate() {
        rows_vec[i % rows as usize].push(item);
    }

    let mut result = Vec::new();
    let mut y = area.y;
    let mut remaining_height = area.height;

    for (ri, row) in rows_vec.iter().enumerate() {
        if row.is_empty() || remaining_height == 0 {
            continue;
        }
        let remaining_rows = (rows_vec.len() - ri) as u16;
        let row_sum: f64 = row.iter().map(|(_, v)| *v).sum();
        let mut height = ((row_sum / total_f) * area.height as f64).round() as u16;
        if height == 0 {
            height = 1;
        }
        let max_height = remaining_height.saturating_sub(remaining_rows.saturating_sub(1));
        if height > max_height {
            height = max_height;
        }
        if ri == rows_vec.len() - 1 || height > remaining_height {
            height = remaining_height;
        }

        let mut x = area.x;
        let mut used = 0u16;
        for (i, (idx, v)) in row.iter().enumerate() {
            let mut width = ((*v / row_sum) * area.width as f64).round() as u16;
            if width == 0 {
                width = 1;
            }
            if i == row.len() - 1 {
                width = area.width.saturating_sub(used);
            }
            result.push(BlockRect {
                index: *idx,
                rect: Rect { x, y, width, height },
            });
            x = x.saturating_add(width);
            used = used.saturating_add(width);
        }

        y = y.saturating_add(height);
        remaining_height = remaining_height.saturating_sub(height);
    }

    result
}

fn worst_ratio_stats(min: f64, max: f64, sum: f64, short: f64) -> f64 {
    if min <= 0.0 || sum <= 0.0 {
        return f64::MAX;
    }
    let s2 = short * short;
    let sum2 = sum * sum;
    (s2 * max / sum2).max(sum2 / (s2 * min))
}

fn layout_row(row: &[(usize, f64)], rect: Rect, is_last: bool) -> (Vec<BlockRect>, Rect) {
    let horizontal = rect.width >= rect.height;
    let mut blocks = Vec::new();
    let row_area: f64 = row.iter().map(|(_, a)| *a).sum();

    if horizontal {
        let mut height = (row_area / rect.width as f64).round() as u16;
        if height == 0 {
            height = 1;
        }
        if height > rect.height {
            height = rect.height;
        }
        if is_last {
            height = rect.height;
        }

        let mut x = rect.x;
        let mut used = 0u16;
        for (i, (idx, area)) in row.iter().enumerate() {
            let mut width = (*area / height as f64).round() as u16;
            if width == 0 {
                width = 1;
            }
            if i == row.len() - 1 {
                width = rect.width.saturating_sub(used);
            }
            blocks.push(BlockRect {
                index: *idx,
                rect: Rect { x, y: rect.y, width, height },
            });
            x = x.saturating_add(width);
            used = used.saturating_add(width);
        }

        let new_rect = Rect {
            x: rect.x,
            y: rect.y.saturating_add(height),
            width: rect.width,
            height: rect.height.saturating_sub(height),
        };
        (blocks, new_rect)
    } else {
        let mut width = (row_area / rect.height as f64).round() as u16;
        if width == 0 {
            width = 1;
        }
        if width > rect.width {
            width = rect.width;
        }
        if is_last {
            width = rect.width;
        }

        let mut y = rect.y;
        let mut used = 0u16;
        for (i, (idx, area)) in row.iter().enumerate() {
            let mut height = (*area / width as f64).round() as u16;
            if height == 0 {
                height = 1;
            }
            if i == row.len() - 1 {
                height = rect.height.saturating_sub(used);
            }
            blocks.push(BlockRect {
                index: *idx,
                rect: Rect { x: rect.x, y, width, height },
            });
            y = y.saturating_add(height);
            used = used.saturating_add(height);
        }

        let new_rect = Rect {
            x: rect.x.saturating_add(width),
            y: rect.y,
            width: rect.width.saturating_sub(width),
            height: rect.height,
        };
        (blocks, new_rect)
    }
}
