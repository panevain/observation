# Using Observation

Observation estimates how much of a street home surveillance cameras can see. You choose an area, place cameras on walls or poles, aim them, and read the coverage figures and colours. For how to run the app, see the [README](../README.md).

On first run (no saved scene) the app loads the demo street with ten example cameras and shows a "Getting started" card. It reopens from the `?` button or the `?` key.

## The screen

| Region | What is in it |
| --- | --- |
| Top bar | App name; the current area ("Demo street" or "OSM area · W × H m"); **Change area...**; **Demo street**; on the right **Import**, **Export**, **Settings** (the gear: the UI font, Inter or Iosevka, remembered in this browser) and **?** |
| Tool rail (left) | Five tools, each an icon with its name and its shortcut key in the corner; the active one is filled with the accent colour: **Select**, **Place**, **Aim**, **Move** and **Region**. Aim and Move are disabled until a camera is selected. Below them, two toggles, which show a sunken fill and an accent underline while on: **Cones** (view-volume outlines for every camera; the selected camera's outline is always shown) and **Walls** (coverage colours on building walls; the ground overlay is always on) |
| 3D viewport | The street in 3D with the map draped on the ground. A small pill at the top left names the active tool, says what a click will do, and shows the selected camera (or, in Place, the model being placed, or in Region the corners so far). The pointer changes with the tool too: an arrow for Select, a plus for Place, a crosshair for Aim, a move cursor for Move and a cell cursor for Region. While a camera is selected (in Select, Aim or Move), a small toolbar floats above it with **Aim**, **Move**, **Focus**, **Duplicate** and **Delete**. Messages appear as toasts at the bottom |
| Inspector (right, resizable) | Sections **Coverage**, **Cameras**, **Selected camera** (only while a camera is selected), **Custom model** (collapsed) and **Analysis** |
| Status bar | The active tool's keys and gestures as keycaps on the left (dropped from the end when the window is narrow); on the right the three coverage figures (street, 2+ cameras, walls), so they stay in view while the inspector scrolls, and the OpenStreetMap attribution link |

## Pick an area

The demo street (a residential street in Juanita, Kirkland WA) is always one click away: **Demo street** in the top bar. It is built into the server and needs no map-data lookup, and it comes with ten example cameras of different makes and types, mounted on house fronts and facing the nearest street.

To load another street:

1. Click **Change area...**. A map dialog opens, centred on the current area.
2. Pan by dragging and zoom with the scroll wheel (or the **-** and **+** buttons).
3. Draw the area: turn on **Draw area** and drag a box, or hold **Shift** and drag. Or click **Square here** for a 300 m square at the map centre.
4. The box shows its size. Each side must be between **20 m and 500 m**; otherwise the box turns red and the reason appears next to the button.
5. Click **Load area**. Close the dialog with **Close**, **Esc**, or by clicking outside it.

While the server fetches and prepares the data, a card in the 3D view says what is loading and for how long. A first request for a new place waits on the public map-data service and can be slow; after a few seconds the card says so, and offers **Use demo street instead**. **Cancel** stops waiting (the request is ignored when it arrives). Repeat requests for the same box are served from the server's cache. If loading fails, for example with "busy, please retry shortly" when the map-data provider is busy, the message stays at the bottom of the view until you dismiss it, with a **Retry** button.

Loading a new area, including **Demo street**, removes the cameras you placed, because cameras belong to an area (the demo street then gets its ten example cameras again). A message with an **Undo** button appears, and **Undo** (or Ctrl+Z) brings the previous area and its cameras back. Undo only lasts for this visit, and autosave overwrites the stored scene within a couple of seconds, so use **Export** if you want to keep a copy.

## Place cameras

1. In the inspector, under **Cameras**, choose a model in **New camera model**. Models are grouped by type (Doorbell, Floodlight, Spotlight, Bullet, Turret, Dome, Indoor) and show their horizontal field of view. Type in the search box at the top of the list to filter by maker, name or type (every word must match); **Enter** picks the first match of what you typed. See the [camera catalogue](camera-catalog.md) for sources. The default is a generic 160 degree doorbell.
2. Press **Place camera...** or **P**.
3. Click where the camera goes:
   - **A wall:** the camera is mounted 0.15 m in front of the wall, at the height you clicked, facing outward. Its tilt is the type's default (doorbell 0 degrees, indoor -10, others -20).
   - **The ground or a roof:** the camera goes on a pole at the model's default mount height above that point (doorbell 1.2 m, indoor 2.0 m, floodlight and spotlight 2.7 m, bullet, turret and dome 2.6 m). It faces north at first, and the tool switches to **Aim** so your next click points it.
   - **An existing camera:** selects it.
4. The new camera becomes the selection. It gets a name ("Camera 1", ...) and the default colour for its type: doorbell blue, floodlight green, spotlight sky blue, bullet indigo, turret teal, dome violet, indoor slate. These stay clear of the yellow-to-purple coverage colours, and each type also has its own icon in the camera list and model picker. The second and third cameras of a type get a lighter and a darker shade of its colour; after that the shades repeat, and the camera names tell them apart.

## Aim and move

- **Aim (A):** click or drag on a wall or the ground; the camera turns to look at that point and keeps its roll. Clicks on the selected camera's own body are ignored.
- **Move (M):** click or drag on a wall or the ground to relocate the selected camera. On a wall it mounts there and turns to face outward, as in Place. On the ground or a roof it moves to the mount height above the point and keeps its heading.
- **Select (V):** click a camera to select it, click empty space to deselect.

With Aim or Move active and a camera selected, a left-drag drives the tool rather than the view. Use right-drag to pan and the wheel to zoom, or switch back to Select to orbit.

## Fine-tune in the inspector

Select a camera (in the viewport or by clicking its row in the list) to open **Selected camera**. It starts with the camera's colour, its type icon and its name (free text, up to 48 characters, edited in place), then:

| Field | Meaning |
| --- | --- |
| Model | Change the model without moving the camera (searchable, as above) |
| Colour | Colour of the camera body, its view cone and its per-camera overlay. Pick any colour or one of the quick swatches; **Default** returns to the colour for the camera's type |
| Position | **E** east, **S** south and **H** height above ground, in metres from the area centre. Height is limited to 0-60 m |
| Orientation | A compass dial: click or drag it to point the camera (Shift snaps to 15 degree steps); the shaded wedge is the horizontal field of view. Beside it, **Heading** (0-360 degrees clockwise from north, with a compass label such as NE; hover the label for the unit vector the camera faces as east, north, up), **Tilt** (-90 to 90 degrees; negative looks down) and **Roll** (-180 to 180 degrees) |
| Optics (collapsed) | Read-only. **Field of view**, **Resolution** and **Night vision** from the model (night vision shows "7.5 m (assumed)" in a warning colour when the manufacturer publishes no figure), **Modelled range** for the current detail level and lighting (see below), and **Source**, a link to the manufacturer's spec page for catalogue models |

Every number field changes when you drag across it: hold **Shift** for finer steps or **Ctrl** (**Cmd** on a Mac) for coarser ones, or click it to type a value.

Buttons: **Aim (A)**, **Move (M)**, **Focus (F)** (frame the camera in the view), **Duplicate** (a copy one metre east, with a new name) and **Delete**.

In the **Cameras** list, each row shows an eye, the camera's colour, its type icon, its name and its model. Click a row to select the camera (click it again to deselect). Hovering a row highlights that camera in the 3D view, and hovering a camera in the 3D view highlights its row. The eye leaves a camera out of the coverage without deleting it (a crossed-out eye; the row is greyed out and the camera is ignored by the statistics and overlays). Right-click a row for **Select**, **Focus**, **Aim**, **Move**, **Duplicate**, **Include in coverage** and **Delete**.

## Read the coverage

The **Coverage** section updates as soon as you change anything. With no cameras placed it shows a short "No cameras yet" note with a **Place camera...** button instead of the figures.

| Figure | Meaning |
| --- | --- |
| **of street seen** | Share of the street surface area that at least one enabled camera can see |
| **by 2+ cameras** | Share of the street surface seen by two or more cameras |
| **of walls seen** | Share of building wall area (every building in the area) that at least one camera can see |

Below the bar, the square metres behind the street figure are shown, plus "open ground seen": all visible ground that is not under a building, which includes the street.

**Limit the figures to a region.** To measure only part of the area, such as your own frontage or one block, click **Draw region...** under the figures (or press **R**). Then click the corners of the area on the ground. Clicking the first corner again (anywhere inside the circle drawn around it), or pressing **Enter**, closes the region; **Backspace** removes the last corner and **Esc** cancels. Corners stay within the loaded area, and the outline must not cross itself. The region is drawn as a dark outline. The heading changes to "Coverage inside region", and every figure (street, walls, open ground) then counts only ground cells whose centre lies inside the outline, and wall patches whose ground half a metre in front lies inside it. **Only inside region** turns the limit off and on without losing the outline (a disabled region is drawn faded). **Redraw** replaces it and **Clear** removes it. The region is saved with the scene and is removed when you load a different area.

What counts as "street": every mapped road, service road (including driveways) and footpath, drawn as a band of its estimated width (about 14 m for a motorway down to 2 m for a footpath; the `width` and `lanes` tags are used when present), excluding anything under a building. Other way types, such as tracks, are ignored. Footpaths and service lanes therefore count towards the street figure.

What counts as "seen": a spot is seen when it is inside the camera's field of view, within the camera's modelled range, and a straight line from the camera to a person-height point above it does not pass through a building. Ground is sampled every 0.5 m and walls in roughly 1 m patches; a wall patch must also face the camera.

**Colours.** Switch with the **Overlap / Per camera** control (or **N**):

- **Overlap:** a heat ramp by number of cameras that see a spot: 1 yellow, 2 orange, 3 red, 4 or more purple. The legend appears under the control.
- **Per camera:** each camera in its own colour (the swatch next to its name; see **Colour** above). Where views overlap, the camera lower in the list is painted on top.

## Analysis settings

These apply to every camera and are saved with the scene.

**Detail level** sets how many pixels cover one metre of a person, using the DORI levels (EN 62676-4):

| Level | Pixels per metre | Meaning |
| --- | --- | --- |
| Detect | 25 | Can tell someone is there (default) |
| Observe | 62 | Clothing and activity |
| Recognise | 125 | A person you know |
| Identify | 250 | A stranger, beyond doubt |

A higher level shrinks each camera's range. The range for a level is `width_px / (2 x px_per_m x tan(hfov / 2))`, capped at 100 m. For example a 1920 px wide, 90 degree camera reaches about 38 m at Detect, 15 m at Observe, 7.7 m at Recognise and 3.8 m at Identify.

**Lighting.** **Day** uses the DORI range only. **Night (IR)** uses the smaller of the DORI range and the camera's night-vision range. If the manufacturer publishes none, the app assumes **7.5 m** (about 25 ft, typical for consumer infrared LEDs) and flags it in the camera's details. The cameras' own floodlights and street lighting are not modelled.

**Subject height** is the height above ground of the point that has to be visible. Presets: **Ground** (0 m), **Torso** (1.0 m, the default) and **Face** (1.6 m); or type a value from 0 to 2.5 m.

## Custom models

Open **Custom model** (above Analysis in the inspector) to add a camera that is not in the catalogue. Fill in name, type, horizontal and vertical field of view (10-180 degrees), resolution (width 160-8192, height 120-8192) and night-vision range (0-60 m; 0 means no night coverage), then click **Add model**. The model is selected for new cameras, appears in the model lists, and is stored with the scene. Models cannot be edited or removed in the app once added; edit an exported scene file and import it if you need to.

## Undo, save, export and import

- **Undo and redo.** The arrows in the top bar, or **Ctrl+Z** and **Ctrl+Shift+Z** (or **Ctrl+Y**; **Cmd** on a Mac), step back and forward through your changes: placing, moving, aiming, editing and deleting cameras, the region, analysis settings, custom models, imports and area changes. A drag or a typed name counts as one step when you let go or leave the field. Hover an arrow to see what it would undo or redo. Deleting a camera or replacing the cameras also shows a message with an **Undo** button. Undo history is kept for this visit only (up to 100 steps), and undoing an area change reloads that area. While a text field has focus, Ctrl+Z undoes typing in the field instead.
- **Autosave.** The scene (area, cameras, custom models, analysis settings and region) is saved to your browser's local storage about every 1.5 seconds under the key `observation.scene.v1`. It is restored the next time you open the app in the same browser. Clearing site data, private windows and some browser settings discard it. The status bar shows **Saved** once a save has gone through, or **Not saved** in a warning colour if the browser refused to store the scene (full, blocked or private mode); then use **Export** to keep your work.
- **Export** downloads `observation-scene.json`. **Import** replaces the current scene with a chosen JSON file, without asking first (it can be undone), and reloads its area from the server.

Scene files are plain JSON:

| Field | Notes |
| --- | --- |
| `format` | Must be `"observation-scene"` |
| `version` | `1`. Files with a newer version are refused |
| `area` | `{"kind": "demo"}` or `{"kind": "osm", "bbox": {"south", "west", "north", "east"}}` |
| `origin` | `{"lat", "lon"}` of the area centre that camera positions are relative to |
| `cameras` | Optional list. Each has `id`, `label`, `model_id`, `position` as `[east, up, south]` in metres, `yaw_deg` (compass bearing), `pitch_deg`, `roll_deg`, `enabled`, and optionally `color` as `[r, g, b]` (0-255; absent means the default for the model's type). The `color_index` field of older files is ignored |
| `custom_models` | Optional list of models: `id`, `vendor`, `name`, `kind`, `hfov_deg`, `vfov_deg`, `resolution` as `[w, h]`, and optionally `ir_range_m`, `source`, `notes` |
| `params` | Optional: `dori` (`Detect`, `Observe`, `Recognise`, `Identify`), `lighting` (`Day`, `Night`), `subject_height_m` |
| `region` | Optional: `points` (corners as `[east, south]` in metres, at most 256) and `enabled` |

Imports are checked: at most 256 cameras and 64 custom models, finite positions and angles within 10,000, subject height 0-10 m, and valid custom models. A spec-sheet `source` link that is not a plain `https://` URL is removed from the model, and a region with more than 256 corners or corners beyond 10,000 m is removed from the scene, rather than failing the import. A bad file shows an error toast and leaves the current scene untouched.

## Keyboard, mouse and touch

| Input | Action |
| --- | --- |
| **V** / **P** / **A** / **M** / **R** | Select / Place / Aim / Move / Region tool |
| **Enter**, **Backspace** (Region tool) | Close the region; remove the last corner |
| Left-drag | Orbit the view (with Aim or Move and a camera selected: aim or move it) |
| Right-drag, or **Shift** + left-drag | Pan (Shift-drag does not pan while Aim or Move is driving a camera; use right-drag) |
| Scroll wheel | Zoom (8 m to 1200 m from the focus point) |
| Click | Select a camera, or use the active tool |
| **F** | Focus the view on the selected camera (moving in to within 60 m if you were farther away) |
| **N** | Toggle Overlap / Per camera colours |
| **Delete** or **Backspace** | Remove the selected camera |
| **Esc** | Return to Select (and, if already in Select, deselect); closes the help and map dialogs |
| **?** (also **/** and **F1**) | Show or hide the help card |
| One finger | Same as left mouse button: tap to click, drag to orbit or drive the tool |
| Two fingers | Pinch to zoom, drag to pan |

Shortcuts are ignored while a text box has focus. The map dialog has its own controls: drag to pan, scroll to zoom, Shift-drag or **Draw area** to draw.

## Limitations and accuracy

Treat the numbers as a rough guide, not a survey.

- **Optics.** Cameras are modelled as rectilinear (pinhole) lenses with the advertised horizontal and vertical field of view, clamped to 170 degrees. Fisheye doorbells (150 degrees and wider) are approximations, and range is computed at the centre of the frame, so it is overstated towards the edges. Range is capped at 100 m. Real image quality also depends on lens distortion, compression, weather and lighting.
- **DORI is an approximation.** The detail levels are pixel-density targets from the published horizontal resolution, not a test of what a camera actually records.
- **Buildings only.** Only OpenStreetMap buildings block the view. Trees, hedges, fences, vehicles, poles, awnings, terrain, bridges and tunnels are not modelled; the ground is flat. Buildings are flat-roofed boxes, so roof pitch and dormers are ignored, and `building:part` outlines are not loaded separately.
- **Heights are inferred.** Use the `height` tag when present (units such as `m`, `ft` and `12'6"` are understood); otherwise `building:levels` at 3 m per storey (plus 1 m for the roof when no roof levels are tagged); otherwise a default for the building type: sheds, garages, carports and cabins 3 m, bungalows 4 m, houses and residential buildings 6.5 m, apartments, dormitories and hotels 10 m, churches 12 m, anything else 6 m. Heights are limited to 1-300 m. Many areas have few height tags, so wall figures in particular can be off.
- **Map data varies.** OpenStreetMap coverage and accuracy differ by place; missing buildings let cameras see through the gap.
- **Night.** The 7.5 m assumption applies only when no night-vision range is published; floodlights and street lights are ignored.
- **One area at a time,** 20-500 m per side. Only the loaded area is analysed, so views beyond its edge are cut off.
- **Single person-height sample.** A spot is judged by one point at the subject height; people of other heights or crouching behind low cover are not distinguished.
- **Browser needs.** WebAssembly and WebGL2 are required. The light or dark theme follows your system setting at page load.
