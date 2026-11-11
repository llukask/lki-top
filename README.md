# lki-top

A graphical system monitor in the style of
[btop](https://github.com/aristocratos/btop), built on
[gpui](https://github.com/zed-industries/zed/tree/main/crates/gpui).
Linux only.

It shows:

- CPU usage per core, with temperature, clock and load. Also AMD GPU
  usage, VRAM, temperature and power, and the output power of a Corsair
  HX…i or RM…i power supply.
- Memory (used, available, cached, free, swap), and each disk's space
  and read and write rates.
- Network traffic for the physical interfaces together, or for one
  interface.
- Processes as a list or a tree, sorted by btop's "cpu lazy" order by
  default, with a filter, signals and a detail panel.

## Running

With Nix:

    nix run github:llukask/lki-top

Without Nix, install a Rust toolchain and the libraries gpui needs
(xkbcommon, xcb, wayland, vulkan-loader, fontconfig, freetype), then:

    cargo run --release

## Using it

Press `?` to list every key. Each key hint on screen also works as a
button, and right-clicking a process opens a menu.

lki-top saves the theme, refresh rates, sort, tree view and network
interface to `$XDG_CONFIG_HOME/lki-top/settings`.

## Themes

`c` and `C` step through the themes. lki-top ships btop's default theme
and the four Catppuccin flavors. It also reads btop `.theme` files from
btop's theme directories and from `~/.config/lki-top/themes`.

## License

MIT. The bundled themes keep their own licenses: btop's default theme
is Apache-2.0 (`themes/btop.LICENSE`), the Catppuccin themes are MIT
(`themes/catppuccin.LICENSE`).
