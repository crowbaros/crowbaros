// SPDX-License-Identifier: MIT
//! Month calendar grid math, quick timer, and co-located bar clock popover view.

pub mod view;

pub use view::{CalendarPopover, toggle};

use chrono::{Datelike, Local, NaiveDate};

pub const WEEKDAY_HEADERS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];
pub const TIMER_PRESETS_SECS: [u32; 4] = [60, 300, 600, 1500];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalendarCell {
    pub day: u32,
    pub in_month: bool,
    pub is_today: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalendarMonth {
    pub year: i32,
    pub month: u32,
}

impl CalendarMonth {
    pub fn current() -> Self {
        let today = Local::now().date_naive();
        Self {
            year: today.year(),
            month: today.month(),
        }
    }

    pub fn prev(self) -> Self {
        if self.month <= 1 {
            Self {
                year: self.year - 1,
                month: 12,
            }
        } else {
            Self {
                year: self.year,
                month: self.month - 1,
            }
        }
    }

    pub fn next(self) -> Self {
        if self.month >= 12 {
            Self {
                year: self.year + 1,
                month: 1,
            }
        } else {
            Self {
                year: self.year,
                month: self.month + 1,
            }
        }
    }

    pub fn title(self) -> String {
        let name = match self.month {
            1 => "January",
            2 => "February",
            3 => "March",
            4 => "April",
            5 => "May",
            6 => "June",
            7 => "July",
            8 => "August",
            9 => "September",
            10 => "October",
            11 => "November",
            12 => "December",
            _ => "Month",
        };
        format!("{name} {}", self.year)
    }
}

pub fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid year/month");
    let next = NaiveDate::from_ymd_opt(next_year, next_month, 1).expect("valid next month");
    (next - first).num_days() as u32
}

/// Builds a 42-cell (6×7) Sunday-first month calendar grid, filling leading
/// days from the previous month and trailing days from the next month.
pub fn month_grid(year: i32, month: u32, today: NaiveDate) -> Vec<CalendarCell> {
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid year/month");
    let leading = first.weekday().num_days_from_sunday() as usize;
    let total_days = days_in_month(year, month);
    let prev = CalendarMonth { year, month }.prev();
    let prev_days = days_in_month(prev.year, prev.month);

    let mut cells = Vec::with_capacity(42);
    for offset in (0..leading).rev() {
        let day = prev_days - offset as u32;
        let date = NaiveDate::from_ymd_opt(prev.year, prev.month, day);
        cells.push(CalendarCell {
            day,
            in_month: false,
            is_today: date == Some(today),
        });
    }
    for day in 1..=total_days {
        let date = NaiveDate::from_ymd_opt(year, month, day);
        cells.push(CalendarCell {
            day,
            in_month: true,
            is_today: date == Some(today),
        });
    }
    let next = CalendarMonth { year, month }.next();
    let mut next_day = 1u32;
    while cells.len() < 42 {
        let date = NaiveDate::from_ymd_opt(next.year, next.month, next_day);
        cells.push(CalendarCell {
            day: next_day,
            in_month: false,
            is_today: date == Some(today),
        });
        next_day += 1;
    }
    cells
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickTimer {
    pub preset_secs: u32,
    pub remaining_secs: u32,
    pub running: bool,
    pub finished: bool,
}

impl Default for QuickTimer {
    fn default() -> Self {
        Self::with_preset(300)
    }
}

impl QuickTimer {
    pub fn with_preset(secs: u32) -> Self {
        Self {
            preset_secs: secs,
            remaining_secs: secs,
            running: false,
            finished: false,
        }
    }

    pub fn select_preset(&mut self, secs: u32) {
        self.preset_secs = secs;
        self.remaining_secs = secs;
        self.running = false;
        self.finished = false;
    }

    pub fn toggle(&mut self) {
        if self.remaining_secs == 0 {
            self.remaining_secs = self.preset_secs;
            self.finished = false;
        }
        self.running = !self.running;
    }

    pub fn reset(&mut self) {
        self.remaining_secs = self.preset_secs;
        self.running = false;
        self.finished = false;
    }

    pub fn tick_one_second(&mut self) -> bool {
        if !self.running {
            return false;
        }
        if self.remaining_secs > 1 {
            self.remaining_secs -= 1;
            true
        } else {
            self.remaining_secs = 0;
            self.running = false;
            self.finished = true;
            true
        }
    }

    pub fn formatted(&self) -> String {
        let mins = self.remaining_secs / 60;
        let secs = self.remaining_secs % 60;
        format!("{mins:02}:{secs:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_navigation_wraps_across_year_boundaries() {
        let jan = CalendarMonth {
            year: 2026,
            month: 1,
        };
        assert_eq!(
            jan.prev(),
            CalendarMonth {
                year: 2025,
                month: 12
            }
        );
        let dec = CalendarMonth {
            year: 2026,
            month: 12,
        };
        assert_eq!(
            dec.next(),
            CalendarMonth {
                year: 2027,
                month: 1
            }
        );
        assert_eq!(jan.title(), "January 2026");
    }

    #[test]
    fn leap_year_and_month_grid_highlight_today() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);

        let today = NaiveDate::from_ymd_opt(2026, 11, 25).unwrap();
        let grid = month_grid(2026, 11, today);
        assert_eq!(grid.len(), 42);
        assert!(grid[0].in_month);
        assert_eq!(grid[0].day, 1);
        let today_cells: Vec<_> = grid.iter().filter(|c| c.is_today).collect();
        assert_eq!(today_cells.len(), 1);
        assert_eq!(today_cells[0].day, 25);
        assert!(today_cells[0].in_month);
        assert!(!grid[30].in_month);
        assert_eq!(grid[30].day, 1);
    }

    #[test]
    fn quick_timer_counts_down_and_stops_at_zero() {
        let mut timer = QuickTimer::with_preset(2);
        assert_eq!(timer.formatted(), "00:02");
        assert!(!timer.tick_one_second());
        timer.toggle();
        assert!(timer.tick_one_second());
        assert_eq!(timer.formatted(), "00:01");
        assert!(timer.tick_one_second());
        assert_eq!(timer.formatted(), "00:00");
        assert!(!timer.running);
        assert!(timer.finished);
        assert!(!timer.tick_one_second());
        timer.reset();
        assert_eq!(timer.formatted(), "00:02");
        assert!(!timer.finished);
    }
}
