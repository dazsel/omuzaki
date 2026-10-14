# Notices for the fonts and icons bundled in omuzaki-ui

These files are compiled into the app (`ds/fonts.rs`). Licences are in `docs/licensing/register.md`; the app's
"About and licences" screen must show this file together with the full generated list.

| What | Where | Licence | Copyright |
|---|---|---|---|
| Instrument Sans (variable, unmodified, from Google Fonts) | `InstrumentSans.ttf` | SIL OFL 1.1, text in `OFL-InstrumentSans.txt` | 2022 The Instrument Sans Project Authors (https://github.com/Instrument/instrument-sans) |
| Bricolage Grotesque (variable, unmodified, from Google Fonts) | `BricolageGrotesque.ttf` | SIL OFL 1.1, text in `OFL-BricolageGrotesque.txt` | 2022 The Bricolage Grotesque Project Authors (https://github.com/ateliertriay/bricolage) |
| JetBrains Mono (variable, unmodified, from Google Fonts) | `JetBrainsMono.ttf` | SIL OFL 1.1, text in `OFL-JetBrainsMono.txt` | 2020 The JetBrains Mono Project Authors (https://github.com/JetBrains/JetBrainsMono) |
| Phosphor Icons (Regular and Fill), via the `egui-phosphor` crate | embedded by the crate | MIT, text below | 2020-2021 Phosphor Icons |

The fonts are not sold on their own and are not renamed or modified: egui 0.36 pins a variable font's axes per family,
so one file serves every weight (`ds/fonts.rs`). None of the three declares a Reserved Font Name.

## Phosphor Icons (MIT)

Copyright (c) 2020-2021 Phosphor Icons

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the Software without restriction, including without limitation the
rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit
persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the
Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE
WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
