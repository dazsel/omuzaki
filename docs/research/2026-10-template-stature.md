# Template stature: how tall each body template stands

Each template declares `default_stature_m`, the height in metres of the person it stands for. The build scales the body and
head uniformly to it (`omuzaki_char_api::stature`). The first numbers (0.95, 1.10, 1.58, 1.72) were rough placeholders; this note replaces them with values derived from public measurements. No third-party character art is used as data.

## Sources (all public domain or public data)

1. **ANSUR II** (US Army anthropometric survey 2012, public domain; 4,082 men, 1,986 women), public data files from
   <https://www.openlab.psu.edu/ansur2/>. Computed here from `ANSUR_II_MALE_Public.csv` and `ANSUR_II_FEMALE_Public.csv`
   (millimetres): mean stature 1756.2 (sd 68.5) men, 1628.5 (sd 64.2) women. The pooled mean over all 6,068 is
   (4082 x 1756.2 + 1986 x 1628.5) / 6068 = **1714.4 mm**. (The same survey is used for the landmark ratios.)
2. **CDC 2000 growth charts, stature-for-age** (US National Center for Health Statistics, public domain). The median (M of the
   LMS table) read from the `cdc_ref_data` table shipped in the CRAN package `cdcanthro` 0.3.0 (the CDC reference file
   `statage`), because cdc.gov refused automated downloads. Median stature, cm, boys / girls:
   13 y 156.4 / 157.3, 14 y 164.1 / 160.5, **15 y 170.1 / 161.9**, 16 y 173.6 / 162.6, 18 y 176.2 / 163.1.
3. **Head-count canon.** The project's heads-tall values: 7.5 for a real adult (the canon, agreeing with ANSUR II:
   Vitruvius says 8) and the anime model-sheet convention of 6 to 7.9 heads measured on the CC0 VRoid samples.

## Decision (2026-10-12): two families

Realistic-proportion templates take real anthropometry; the SD (super-deformed) templates keep the stylised head of the
tuned prototype.

- **soft-sd builds at its authored height, scale exactly 1.0** (about 0.966 m, computed from the template). soft-sd is the
  prototype port: its geometry has T-GEOM parity, and its springs, hair and cloth were tuned by eye at that size. Shrinking it
  to a real-head 0.69 m would change every tuned motion (faster spring periods, less sag) with no reference to judge against.
- **mini shares soft-sd's head size.** The SD templates are one stylised family with one head, so mini's stature is soft-sd's
  head height (0.383 m, chin to crown) times mini's head count: 3 x 0.383 = 1.149 m, scale 1.0 as well.
- **Why two families:** a real-size head on a 2.5-head body makes a toddler-sized doll whose physics nobody tuned.

(A first draft derived both from the teen's head, 0.2767 m, giving 0.692 and 0.830 m. The decision above replaces it.)

## Method

A template is written in head units, so its stature is `heads_tall x the height of one head`.

- **tall (7.5 heads) = a real adult.** The measured adult mean: pooled ANSUR II 1714.4 mm, **1.714 m** (one head 0.2285 m, in
  line with a real adult head, chin to crown about 0.23 m).
- **standard (6 heads) = a teenager.** 15 years is the middle of the teen range (13 to 17); averaging the sexes' CDC medians,
  (170.1 + 161.9) / 2 = 166.0 cm, **1.660 m**.
- **soft-sd, mini = the SD family**, as decided above: authored size, scale 1.0.

## Values

| Template | Heads | Stature (m) | Scale from authored height | Placeholder (m) |
|---|---|---|---|---|
| soft-sd | 2.5 | 0.966 (authored) | 1.000 | 0.95 |
| mini | 3 | 1.149 (3 x 0.383) | 1.000 | 1.10 |
| standard | 6 | 1.660 | 0.722 | 1.58 |
| tall | 7.5 | 1.714 | 0.597 | 1.72 |

The adult and the teen are within 8 cm of the placeholders. The SD templates are 1 to 5 cm from theirs; their value is not
a measurement of a person but the stylised head of the family times the head count.

## Caveats

- Statures of the realistic templates are 1 mm precision, not more: the sources are population means with sd of 6 to 7 cm. The body-shape `height` axis
  (+-12% of the body below the head) moves a character within that spread.
- The pooled adult mean mixes the sexes by the survey's own sample (67% men) and the teen by equal weights; per-sex values
  can be used later when the template gains a sex-specific body.
