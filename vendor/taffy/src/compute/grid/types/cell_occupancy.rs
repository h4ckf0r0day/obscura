//! Contains CellOccupancyMatrix used to track occupied cells during grid placement
use super::TrackCounts;
use crate::compute::grid::OriginZeroLine;
use crate::geometry::AbsoluteAxis;
use crate::geometry::Line;
use crate::util::sys::Vec;
use core::cmp::max;
use core::fmt::Debug;
use core::ops::Range;
use grid::Grid;

/// The occupancy state of a single grid cell
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
pub(crate) enum CellOccupancyState {
    #[default]
    /// Indicates that a grid cell is unoccupied
    Unoccupied,
    /// Indicates that a grid cell is occupied by a definitely placed item
    DefinitelyPlaced,
    /// Indicates that a grid cell is occupied by an item that was placed by the auto placement algorithm
    AutoPlaced,
}

/// A dynamically sized matrix (2d grid) which tracks the occupancy of each grid cell during auto-placement
/// It also keeps tabs on how many tracks there are and which tracks are implicit and which are explicit.
pub(crate) struct CellOccupancyMatrix {
    /// The grid of occupancy states
    inner: Grid<CellOccupancyState>,
    /// The counts of implicit and explicit columns
    columns: TrackCounts,
    /// The counts of implicit and explicit rows
    rows: TrackCounts,
}

/// Debug impl that represents the matrix in a compact 2d text format
impl Debug for CellOccupancyMatrix {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(
            f,
            "Rows: neg_implicit={} explicit={} pos_implicit={}",
            self.rows.negative_implicit, self.rows.explicit, self.rows.positive_implicit
        )?;
        writeln!(
            f,
            "Cols: neg_implicit={} explicit={} pos_implicit={}",
            self.columns.negative_implicit, self.columns.explicit, self.columns.positive_implicit
        )?;
        writeln!(f, "State:")?;

        for row_idx in 0..self.inner.rows() {
            for cell in self.inner.iter_row(row_idx) {
                let letter = match *cell {
                    CellOccupancyState::Unoccupied => '_',
                    CellOccupancyState::DefinitelyPlaced => 'D',
                    CellOccupancyState::AutoPlaced => 'A',
                };
                write!(f, "{letter}")?;
            }
            writeln!(f)?;
        }

        Ok(())
    }
}

impl CellOccupancyMatrix {
    /// Create a CellOccupancyMatrix given a set of provisional track counts. The grid can expand as needed to fit more tracks,
    /// the provisional track counts represent a best effort attempt to avoid the extra allocations this requires.
    pub fn with_track_counts(columns: TrackCounts, rows: TrackCounts) -> Self {
        Self { inner: Grid::new(rows.len(), columns.len()), rows, columns }
    }

    /// Determines whether the specified area fits within the tracks currently represented by the matrix
    pub fn is_area_in_range(
        &self,
        primary_axis: AbsoluteAxis,
        primary_range: Range<i16>,
        secondary_range: Range<i16>,
    ) -> bool {
        if primary_range.start < 0 || primary_range.end > self.track_counts(primary_axis).len() as i16 {
            return false;
        }
        if secondary_range.start < 0 || secondary_range.end > self.track_counts(primary_axis.other_axis()).len() as i16
        {
            return false;
        }
        true
    }

    /// Expands the grid (potentially in all 4 directions) in order to ensure that the specified range fits within the allocated space
    fn expand_to_fit_range(&mut self, row_range: Range<i16>, col_range: Range<i16>) {
        // Calculate number of rows and columns missing to accommodate ranges (if any)
        let req_negative_rows = max(-row_range.start, 0);
        let req_positive_rows = max(row_range.end - self.rows.len() as i16, 0);
        let req_negative_cols = max(-col_range.start, 0);
        let req_positive_cols = max(col_range.end - self.columns.len() as i16, 0);

        let old_row_count = self.rows.len();
        let old_col_count = self.columns.len();
        let new_row_count = old_row_count + (req_negative_rows + req_positive_rows) as usize;
        let new_col_count = old_col_count + (req_negative_cols + req_positive_cols) as usize;

        let mut data = Vec::with_capacity(new_row_count * new_col_count);

        // Push new negative rows
        for _ in 0..(req_negative_rows as usize * new_col_count) {
            data.push(CellOccupancyState::Unoccupied);
        }

        // Push existing rows
        for row in 0..old_row_count {
            // Push new negative columns
            for _ in 0..req_negative_cols {
                data.push(CellOccupancyState::Unoccupied);
            }
            // Push existing columns
            for col in 0..old_col_count {
                data.push(*self.inner.get(row, col).unwrap());
            }
            // Push new positive columns
            for _ in 0..req_positive_cols {
                data.push(CellOccupancyState::Unoccupied);
            }
        }

        // Push new negative rows
        for _ in 0..(req_positive_rows as usize * new_col_count) {
            data.push(CellOccupancyState::Unoccupied);
        }

        // Update self with new data
        self.inner = Grid::from_vec(data, new_col_count);
        self.rows.negative_implicit += req_negative_rows as u16;
        self.rows.positive_implicit += req_positive_rows as u16;
        self.columns.negative_implicit += req_negative_cols as u16;
        self.columns.positive_implicit += req_positive_cols as u16;
    }

    /// Mark an area of the matrix as occupied, expanding the allocated space as necessary to accommodate the passed area.
    pub fn mark_area_as(
        &mut self,
        primary_axis: AbsoluteAxis,
        primary_span: Line<OriginZeroLine>,
        secondary_span: Line<OriginZeroLine>,
        value: CellOccupancyState,
    ) {
        let (row_span, column_span) = match primary_axis {
            AbsoluteAxis::Horizontal => (secondary_span, primary_span),
            AbsoluteAxis::Vertical => (primary_span, secondary_span),
        };

        let mut col_range = self.columns.oz_line_range_to_track_range(column_span);
        let mut row_range = self.rows.oz_line_range_to_track_range(row_span);

        // Check that if the resolved ranges fit within the allocated grid. And if they don't then expand the grid to fit
        // and then re-resolve the ranges once the grid has been expanded as the resolved indexes may have changed
        let is_in_range = self.is_area_in_range(AbsoluteAxis::Horizontal, col_range.clone(), row_range.clone());
        if !is_in_range {
            self.expand_to_fit_range(row_range.clone(), col_range.clone());
            col_range = self.columns.oz_line_range_to_track_range(column_span);
            row_range = self.rows.oz_line_range_to_track_range(row_span);
        }

        for x in row_range {
            for y in col_range.clone() {
                *self.inner.get_mut(x as usize, y as usize).unwrap() = value;
            }
        }
    }

    /// Determines whether a grid area specified by the bounding grid lines in OriginZero coordinates
    /// is entirely unnocupied. Returns true if all grid cells within the grid area are unnocupied, else false.
    pub fn line_area_is_unoccupied(
        &self,
        primary_axis: AbsoluteAxis,
        primary_span: Line<OriginZeroLine>,
        secondary_span: Line<OriginZeroLine>,
    ) -> bool {
        let primary_range = self.track_counts(primary_axis).oz_line_range_to_track_range(primary_span);
        let secondary_range = self.track_counts(primary_axis.other_axis()).oz_line_range_to_track_range(secondary_span);
        self.track_area_is_unoccupied(primary_axis, primary_range, secondary_range)
    }

    /// Determines whether a grid area specified by a range of indexes into this CellOccupancyMatrix
    /// is entirely unnocupied. Returns true if all grid cells within the grid area are unnocupied, else false.
    pub fn track_area_is_unoccupied(
        &self,
        primary_axis: AbsoluteAxis,
        primary_range: Range<i16>,
        secondary_range: Range<i16>,
    ) -> bool {
        let (row_range, col_range) = match primary_axis {
            AbsoluteAxis::Horizontal => (secondary_range, primary_range),
            AbsoluteAxis::Vertical => (primary_range, secondary_range),
        };

        // Search for occupied cells in the specified area. Out of bounds cells are considered unoccupied.
        for x in row_range {
            for y in col_range.clone() {
                match self.inner.get(x as usize, y as usize) {
                    None | Some(CellOccupancyState::Unoccupied) => continue,
                    _ => return false,
                }
            }
        }

        true
    }

    /// Determines whether the specified row contains any items
    pub fn row_is_occupied(&self, row_index: usize) -> bool {
        if row_index >= self.inner.rows() {
            return false;
        }
        self.inner.iter_row(row_index).any(|cell| !matches!(cell, CellOccupancyState::Unoccupied))
    }

    /// Determines whether the specified column contains any items
    pub fn column_is_occupied(&self, column_index: usize) -> bool {
        if column_index >= self.inner.cols() {
            return false;
        }
        self.inner.iter_col(column_index).any(|cell| !matches!(cell, CellOccupancyState::Unoccupied))
    }

    /// Returns the track counts of this CellOccunpancyMatrix in the relevant axis
    pub fn track_counts(&self, track_type: AbsoluteAxis) -> &TrackCounts {
        match track_type {
            AbsoluteAxis::Horizontal => &self.columns,
            AbsoluteAxis::Vertical => &self.rows,
        }
    }

    /// Given an axis and a track index
    /// Search backwards from the end of the track and find the last grid cell matching the specified state (if any)
    /// Return the index of that cell or None.
    pub fn last_of_type(
        &self,
        track_type: AbsoluteAxis,
        start_at: OriginZeroLine,
        kind: CellOccupancyState,
    ) -> Option<OriginZeroLine> {
        let track_counts = self.track_counts(track_type.other_axis());
        let track_computed_index = track_counts.oz_line_to_next_track(start_at);

        let maybe_index = match track_type {
            AbsoluteAxis::Horizontal => {
                if track_computed_index < 0 || track_computed_index >= self.inner.rows() as i16 {
                    // Index out of bounds: no tracks to search
                    None
                } else {
                    self.inner.iter_row(track_computed_index as usize).rposition(|item| *item == kind)
                }
            }
            AbsoluteAxis::Vertical => {
                if track_computed_index < 0 || track_computed_index >= self.inner.cols() as i16 {
                    // Index out of bounds: no tracks to search
                    None
                } else {
                    self.inner.iter_col(track_computed_index as usize).rposition(|item| *item == kind)
                }
            }
        };

        maybe_index.map(|idx| track_counts.track_to_prev_oz_line(idx as u16))
    }

    /// Given an axis and a track index
    /// Search forwards from the start of the track and find the first grid cell matching the specified state (if any)
    /// Return the index of that cell or None.
    pub fn first_of_type(
        &self,
        track_type: AbsoluteAxis,
        start_at: OriginZeroLine,
        kind: CellOccupancyState,
    ) -> Option<OriginZeroLine> {
        let track_counts = self.track_counts(track_type.other_axis());
        let track_computed_index = track_counts.oz_line_to_next_track(start_at);

        let maybe_index = match track_type {
            AbsoluteAxis::Horizontal => {
                if track_computed_index < 0 || track_computed_index >= self.inner.rows() as i16 {
                    // Index out of bounds: no tracks to search
                    None
                } else {
                    self.inner.iter_row(track_computed_index as usize).position(|item| *item == kind)
                }
            }
            AbsoluteAxis::Vertical => {
                if track_computed_index < 0 || track_computed_index >= self.inner.cols() as i16 {
                    // Index out of bounds: no tracks to search
                    None
                } else {
                    self.inner.iter_col(track_computed_index as usize).position(|item| *item == kind)
                }
            }
        };

        maybe_index.map(|idx| track_counts.track_to_prev_oz_line(idx as u16))
    }
}
