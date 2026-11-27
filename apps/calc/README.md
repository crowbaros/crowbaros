# Calculator

Calculator (`crowbaros-calc`, `com.crowbaros.Calc`) is a native GPUI app with
Standard, Scientific, and Programmer modes. It follows the saved CrowbarOS
appearance while open and uses `crowbar-ui` controls, Manrope labels, and
IBM Plex Mono for expressions, results, and history.

Type directly into the expression field or use the keys. The answer updates
as you type. Enter keeps the expression in the history tape; selecting an
entry restores its expression, mode, and word width. History holds the last
100 calculations for this window and is not saved to disk.

| Keys | Action |
| --- | --- |
| Alt+1 / Alt+2 / Alt+3 | Standard / Scientific / Programmer |
| Enter or Ctrl+Enter | Add the answer to history |
| Ctrl+C | Copy the current answer, including while editing |
| Ctrl+L | Focus the expression |
| Escape | Clear the expression |
| Tab / Shift+Tab | Move among controls and history |
| Enter / Space on a control | Activate it |
| Ctrl+Q | Close |

Scientific mode exposes `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `sqrt`,
`ln`, `log`, `exp`, `pi`, and `e`; `abs` is also accepted. Trigonometry uses
radians, `ln` is natural logarithm, and `log` is base 10. `^` is a power,
right-associative and evaluated before unary minus. `%` is remainder.
Standard and Scientific share the same floating-point evaluator (IEEE 754
binary64); they differ in the keys shown. Floating-point rounding is visible
rather than hidden behind an implied arbitrary-precision result.

Programmer uses exact unsigned 8-, 16-, 32-, or 64-bit words. Decimal is the
default input; `0x`, `0o`, and `0b` select hexadecimal, octal, and binary.
All four representations update together. `AND`, `OR`, `XOR`, `NOT`, `<<`,
and `>>` are supported (`&`, `|`, and `~` are aliases). `^` remains power;
use `XOR` for exclusive OR. `NOT`, unary minus (two's complement), and left
shifts discard bits outside the selected width. Arithmetic overflow,
underflow, division by zero, fractional input, and shift counts at least as
large as the width produce errors. A right shift fills with zeroes.

## Quick math in the launcher

Press Super and type `1280 * 1.25` or `0xff + 16`. The launcher displays a
live answer; Enter or a click copies it and closes the launcher. Ordinary
application names still search the app catalog. A leading `=` explicitly
requests math, including a single number. The launcher uses the same real
number evaluator as Standard and Scientific.

## Build and run on Void

With Base's GPUI build dependencies installed, from this directory:

```sh
python3 ../shell/packaging/prepare-gpui.py
cargo build --release --locked
cargo test --locked
target/release/crowbaros-calc --version
target/release/crowbaros-calc
```

The pure evaluator lives in [`libs/crowbar-calc`](../../libs/crowbar-calc/).
It has no dependencies or IO. `cargo test --locked` there tests parsing,
precedence, scientific functions, integer operations, errors, resource
limits, and launcher expression detection. `cargo test --locked
--no-default-features` here tests history without GPUI.

From the repository root, `lab/void/desktop_check.py` builds and tests the
app and evaluator in Base's Void build root. It launches Calculator on
Fulcrum, types calculations in each mode, reads copied answers from another
Wayland client, exercises launcher math, and retains Dark and Light captures
with the run's `desktop.log` and `results.json`.
