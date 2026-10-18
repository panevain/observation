# Camera catalog

## Purpose

`crates/observation-client/assets/catalog.ron` is the curated list of consumer
home-surveillance cameras that users can place on a street map. Each entry carries
the few optics numbers needed to draw a coverage footprint: field of view,
main-stream resolution and night-vision range. The file is embedded in the client
and parsed with `observation_core::camera::Catalog::from_ron`, which validates ids,
FOV, resolution and IR ranges and rejects duplicate ids. The integration test
`crates/observation-core/tests/catalog.rs` checks the shipped file.

Three vendor-neutral archetypes (`generic-doorbell-160`, `generic-floodlight-130`,
`generic-bullet-90`) have an empty vendor and no source. They hold mid-market values
for cameras that are not in the list.

## Field reference

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | Stable, unique kebab-case id (`[a-z0-9-]`). Saved scenes refer to it, so never rename a shipped id. |
| `vendor` | string | Manufacturer. Empty for generic archetypes. |
| `name` | string | Product name without the vendor prefix. |
| `kind` | enum | `Doorbell`, `Floodlight`, `Spotlight`, `Bullet`, `Turret`, `Dome` or `Indoor`. Sets default mount height and tilt. |
| `hfov_deg` | float | Horizontal field of view, degrees. |
| `vfov_deg` | float | Vertical field of view, degrees. |
| `resolution` | `(w, h)` | Main-stream size in pixels, in the orientation the camera ships with (portrait doorbells have `h > w`). |
| `ir_range_m` | `Option<float>` | Advertised night-vision distance in metres (feet x 0.3048, 1 decimal). `None` means the manufacturer publishes no figure. |
| `source` | `Option<string>` | `https://` link to the manufacturer spec, tech-specs or support page. Required for every model with a vendor. |
| `notes` | `Option<string>` | Short caveats: derived values, lens variants, conflicting figures. |

The file is sorted by kind (enum order), then vendor, then name.

## How effective range is derived

The app uses DORI pixel-density targets (EN 62676-4):

| Level | Pixels per metre |
| --- | --- |
| Detect | 25 |
| Observe | 62 |
| Recognise | 125 |
| Identify | 250 |

For a level with density `ppm`:

```
range = width_px / (2 * ppm * tan(hfov / 2))      capped at 100 m
```

- **Night:** the range is the smaller of the DORI range and `ir_range_m`. A model
  with `ir_range_m: None` has zero modelled range at night, because the catalog
  never guesses a night-vision distance.
- **Wide lenses:** the model is rectilinear, so FOVs above 170 degrees are clamped
  to 170. Fisheye doorbells (150 degrees and wider) are therefore approximations,
  and the centre-of-frame pixel density overstates range toward the edges.
- Only the horizontal FOV and width enter the range calculation. The vertical FOV
  shapes the footprint on the ground.

## Sourcing policy

- Take values from the manufacturer's spec, tech-specs or support page, and put
  that URL in `source`. Retailer listings and reviews are not sources. Use them only
  to find the manufacturer page.
- Only link URLs you have opened. Never construct one.
- Convert feet to metres (x 0.3048) and round to 1 decimal.
- If the manufacturer publishes only a diagonal FOV `d`, derive H and V from the
  resolution `w x h`:
  `h_fov = 2 * atan(tan(d/2) * w / sqrt(w^2 + h^2))`, and the same with `h` in
  place of `w` for V. Write "derived from diagonal" in `notes`.
  Do not derive when the manufacturer publishes H and V. Use those as published,
  even where the lens is fisheye and the figures disagree with the rectilinear
  formula (the Tapo D230S1 is an example).
- If the pages disagree on a value, use the manufacturer's spec sheet and mention
  the conflict in `notes`. If the value cannot be verified, leave the model out.
- Do not use a night-vision figure from a retailer. Use `None` instead.
- The header comment of `catalog.ron` records the last-verified month. Update it
  whenever you re-check entries.

### Adding or updating a model

1. Find the manufacturer's spec page and read FOV, main-stream resolution and
   night-vision range from it.
2. Add the entry in sorted position (kind, vendor, name), with a unique kebab-case
   `id` of the form `vendor-product-name`.
3. Update the table below so that it matches the RON.
4. Run `cargo test -p observation-core --test catalog`.
5. Bump the "Last verified" month in the RON header if you re-checked other entries.

PR checklist:

- [ ] `source` is a manufacturer `https://` page that you opened
- [ ] H/V FOV, resolution and IR range match that page
- [ ] feet converted to metres, 1 decimal
- [ ] derived values say "derived from diagonal" in `notes`
- [ ] lens or SKU variant named in `notes` where the page lists several
- [ ] entry sorted by kind, vendor, name; `id` unique and kebab-case
- [ ] this document's table updated
- [ ] catalog test passes

## Shipped models

Last verified: 2026-10. IR range "—" means the manufacturer publishes no
night-vision distance (`None`).

| Vendor | Model | Kind | H × V FOV (°) | Resolution | IR range (m) |
| --- | --- | --- | --- | --- | --- |
| (generic) | Generic doorbell (160°) | Doorbell | 160.0 × 120.0 | 1920×1440 | 5.0 |
| Blink | Video Doorbell | Doorbell | 135.0 × 80.0 | 1920×1080 | — |
| Google | Nest Doorbell (battery) | Doorbell | 124.6 × 137.0 | 960×1280 | 3.0 |
| Reolink | Video Doorbell WiFi | Doorbell | 135.0 × 100.0 | 2560×1920 | — |
| Ring | Battery Doorbell Plus | Doorbell | 150.0 × 150.0 | 1536×1536 | — |
| TP-Link | Tapo D230S1 | Doorbell | 135.0 × 102.8 | 2560×1920 | 7.6 |
| Wyze | Video Doorbell Pro | Doorbell | 150.0 × 150.0 | 1440×1440 | — |
| (generic) | Generic floodlight (130°) | Floodlight | 130.0 × 75.0 | 1920×1080 | 9.0 |
| Google | Nest Cam with floodlight (wired) | Floodlight | 123.7 × 92.9 | 1920×1080 | 6.1 |
| Ring | Floodlight Cam Wired Plus | Floodlight | 140.0 × 80.0 | 1920×1080 | — |
| Wyze | Cam Floodlight Pro | Floodlight | 170.0 × 95.0 | 2560×1440 | — |
| Ring | Spotlight Cam Plus | Spotlight | 140.0 × 80.0 | 1920×1080 | — |
| (generic) | Generic bullet (90°) | Bullet | 90.0 × 50.0 | 2560×1440 | 25.0 |
| Reolink | RLC-810A | Bullet | 87.0 × 44.0 | 3840×2160 | 30.0 |
| TP-Link | Tapo C320WS | Bullet | 97.0 × 54.0 | 2560×1440 | 29.9 |
| Ubiquiti | UniFi G5 Bullet | Bullet | 84.4 × 45.4 | 2688×1512 | 9.0 |

Notes on specific entries:

- **Derived from diagonal:** Google Nest Doorbell (battery), 145 degrees diagonal on a
  960x1280 portrait frame, and Google Nest Cam with floodlight (wired), 130 degrees
  diagonal on 1920x1080.
- **Published H/V used as is:** the Wyze Cam Floodlight Pro (180 degrees diagonal),
  TP-Link Tapo D230S1 (166 degrees diagonal; the product page also quotes 160),
  Tapo C320WS (113 degrees diagonal) and Ubiquiti G5 Bullet (99.8 degrees diagonal)
  all publish H and V as well.
- **Lens variant:** the Reolink RLC-810A is listed with the 4.0 mm lens (87 x 44).
  The 2.8 mm version is 101 x 55 and the 6.0 mm version is 53 x 33.
- **Resolution:** Ring, Blink and Nest Cam spec pages say "1080p"; this is stored as
  1920x1080.
- **Not shipped:** eufy Video Doorbell E340 (its pages give "160 degrees" without saying
  which axis) and Arlo Pro 5S 2K (the datasheet says 2048x1080 while other Arlo
  material says 2560x1440). Add them once the manufacturer states the values
  unambiguously.
