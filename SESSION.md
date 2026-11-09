# Session notes

- `libc` is pinned to 0.2.186 in `Cargo.lock`. 0.2.191 dropped
  `libc::ENOATTR` on Linux, which breaks `xattr` 0.2.3, pulled in by
  gpui 0.2.2. A plain `cargo update` brings the breakage back.
- sysinfo 0.40 is out; the code targets 0.39.
- After a signal is sent with a filter active, the footer shows the
  filter text, not the "Sent SIGTERM ..." result. The status also never
  expires.
- Column widths in the process table assume ~7.4 px per monospace
  character at `text_xs` (`CH` in `monitor.rs`). A font with a
  different advance will make the fixed columns too wide or too narrow.
- Missing compared to btop: per-process detail view, light theme,
  per-interface network selection, disk I/O rates, battery.
- Tree view folds only with `space`. Clicking the `▸`/`▾` marker selects
  the row but doesn't fold it.
- The refresh interval resets to 1 s on every start. Nothing persists
  settings yet.
- The process list re-renders every row on screen once per second, and
  `ProcessTable::update` lower-cases every name and command line for
  each filter keystroke. Fine at ~600 processes; untested at 10k.
- Graphs jump one sample at a time instead of scrolling smoothly.
  Smooth scrolling needs a redraw every frame, which costs GPU and CPU
  while the monitor is idle; worth it only as an opt-in.
- Graph fill opacity (`FILL_OPACITY`), line widths and the grid color
  were tuned by eye on one dark screen at scale 1.0. Not checked on
  HiDPI.
