//! The server's log lines, kept in memory as well as printed: the admin page's live log reads them from here.

use std::collections::VecDeque;
use std::sync::Mutex;

/// How many recent lines the admin page can see.
const KEEP: usize = 2000;

struct Lines {
    next: u64,
    lines: VecDeque<(u64, i64, String)>,
}

static LINES: Mutex<Lines> = Mutex::new(Lines { next: 1, lines: VecDeque::new() });

/// Print a log line and keep it for the admin page.
macro_rules! say {
    ($($t:tt)*) => {{
        let line = format!($($t)*);
        println!("{line}");
        $crate::logbuf::push(line);
    }};
}

pub fn push(line: String) {
    let now = crate::game::now_secs();
    let mut l = LINES.lock().unwrap();
    let seq = l.next;
    l.next += 1;
    for part in line.lines() {
        l.lines.push_back((seq, now, part.to_string()));
    }
    while l.lines.len() > KEEP {
        l.lines.pop_front();
    }
}

/// Lines after `after` (a number from an earlier answer), oldest first, at most `max`, and the newest number.
pub fn since(after: u64, max: usize) -> (Vec<(u64, i64, String)>, u64) {
    let l = LINES.lock().unwrap();
    let newer: Vec<_> = l.lines.iter().filter(|(seq, _, _)| *seq > after).cloned().collect();
    let skip = newer.len().saturating_sub(max);
    (newer.into_iter().skip(skip).collect(), l.next - 1)
}
