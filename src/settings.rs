//! What lki-top remembers between runs, in
//! `$XDG_CONFIG_HOME/lki-top/settings` as `key = value` lines:
//!
//! ```text
//! theme = catppuccin_mocha
//! process_interval_ms = 1000
//! graphs_per_refresh = 2
//! sort = cpu lazy
//! sort_descending = true
//! tree = false
//! ```
//!
//! The file is written whole on every change. Reading it is forgiving:
//! unknown keys and values that don't parse fall back to the default for
//! that key, so a hand edit gone wrong costs one setting, not all.

use std::path::PathBuf;

use crate::procs::{Layout, SortColumn, SortOrder};
use crate::rates::Rates;

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// A theme name; the first theme when absent or no longer installed.
    pub theme: Option<String>,
    pub rates: Rates,
    pub sort: SortOrder,
    pub layout: Layout,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: None,
            rates: Rates::DEFAULT,
            sort: SortOrder::by(SortColumn::CpuLazy),
            layout: Layout::Flat,
        }
    }
}

impl Settings {
    /// The saved settings, or the defaults when there is no file yet.
    pub fn load() -> Self {
        file()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map_or_else(Self::default, |text| Self::parse(&text))
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = file()
            .ok_or_else(|| std::io::Error::other("neither XDG_CONFIG_HOME nor HOME is set"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.format())
    }

    fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        let mut interval_ms = settings.rates.process_interval_ms();
        let mut graphs_per_refresh = settings.rates.wanted_graphs_per_refresh();
        let mut column = settings.sort.column;
        let mut descending = None;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "theme" if !value.is_empty() => settings.theme = Some(value.to_owned()),
                "process_interval_ms" => interval_ms = value.parse().unwrap_or(interval_ms),
                "graphs_per_refresh" => {
                    graphs_per_refresh = value.parse().unwrap_or(graphs_per_refresh);
                }
                "sort" => column = SortColumn::from_label(value).unwrap_or(column),
                "sort_descending" => descending = value.parse().ok(),
                "tree" => {
                    if let Ok(tree) = value.parse::<bool>() {
                        settings.layout = if tree { Layout::Tree } else { Layout::Flat };
                    }
                }
                _ => {}
            }
        }
        settings.rates =
            Rates::from_saved(interval_ms, graphs_per_refresh).unwrap_or(settings.rates);
        // A sort without a saved direction gets that column's natural one.
        settings.sort = SortOrder::by(column);
        if let Some(descending) = descending {
            settings.sort.descending = descending;
        }
        settings
    }

    fn format(&self) -> String {
        let mut text = String::new();
        if let Some(theme) = &self.theme {
            text += &format!("theme = {theme}\n");
        }
        text += &format!(
            "process_interval_ms = {}\n\
             graphs_per_refresh = {}\n\
             sort = {}\n\
             sort_descending = {}\n\
             tree = {}\n",
            self.rates.process_interval_ms(),
            self.rates.wanted_graphs_per_refresh(),
            self.sort.column.label(),
            self.sort.descending,
            self.layout == Layout::Tree,
        );
        text
    }
}

fn file() -> Option<PathBuf> {
    crate::theme::config_home().map(|config| config.join("lki-top/settings"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_setting() {
        let settings = Settings {
            theme: Some("catppuccin_mocha".into()),
            rates: Rates::DEFAULT
                .step_process_interval(1)
                .step_graphs_per_refresh(2),
            sort: SortOrder::by(SortColumn::Memory).toggled(SortColumn::Memory),
            layout: Layout::Tree,
        };
        assert_eq!(Settings::parse(&settings.format()), settings);
    }

    #[test]
    fn bad_lines_cost_only_their_own_setting() {
        let settings = Settings::parse(
            "theme = gruvbox_dark\n\
             process_interval_ms = 1234\n\
             sort = nonsense\n\
             tree = yes please\n\
             # a comment\n\
             colour = blue\n",
        );
        let defaults = Settings::default();
        assert_eq!(settings.theme.as_deref(), Some("gruvbox_dark"));
        assert_eq!(settings.rates, defaults.rates);
        assert_eq!(settings.sort, defaults.sort);
        assert_eq!(settings.layout, defaults.layout);
    }

    #[test]
    fn a_sort_without_direction_uses_the_natural_one() {
        let settings = Settings::parse("sort = program\n");
        assert_eq!(settings.sort, SortOrder::by(SortColumn::Name));
        assert!(!settings.sort.descending);
    }
}
