# Using Observation

Observation estimates how much of a street home surveillance cameras can see. You choose an area, place cameras on walls or poles, aim them, and read the coverage figures and colours. For how to run the app, see the [README](../README.md).

On first run (no saved scene) the app loads the demo street with ten example cameras and shows a "Getting started" card. It reopens from the `?` button or the `?` key.

## The screen

| Region | What is in it |
| --- | --- |
| Top bar | App name; the current area ("Demo street" or "OSM area · W × H m"); **Change area...**; **Demo street**; on the right the **Scenes** button (with the open scene's name), **Import**, **Export**, **Sheet…** (an evidence sheet; see [Export an evidence sheet](#export-an-evidence-sheet)), **Connect agent** (when the server offers it; see [Connect an AI agent](#connect-an-ai-agent)), **Settings** (the gear: the UI font, Inter or Iosevka, and whether an Address tool click jumps to the camera list, both remembered in this browser) and **?** |
| Tool rail (left) | Seven tools, each an icon with its name and its shortcut key in the corner; the active one is filled with the accent colour: **Select**, **Place**, **Aim**, **Move**, **Areas**, **Target** and **Address**. Aim and Move are disabled until a camera is selected. Below them, two toggles, which show a sunken fill and an accent underline while on: **Cones** (view-volume outlines for every camera; the selected camera's outline is always shown) and **Walls** (coverage colours on building walls; the ground overlay is always on) |
| 3D viewport | The street in 3D with the map draped on the ground. A small pill at the top left names the active tool, says what a click will do, and shows the selected camera (or, in Place, the model being placed, in Areas the corners so far and what closing the outline will do, in Target what a click marks, or in Address the selected address). The pointer changes with the tool too: an arrow for Select, a plus for Place, a crosshair for Aim, a move cursor for Move, a cell cursor for Areas and Target and a question-mark cursor for Address. While a camera is selected (in Select, Aim or Move), a small toolbar floats above it with **Aim**, **Move**, **Focus**, **Duplicate** and **Delete**. Messages appear as toasts at the bottom |
| Inspector (right, resizable) | At the top, always in view: **Coverage** (the figures, a **Limit** menu, what the figures leave out, a line with the analysis settings that opens the Analysis tab, and the date of the map data; see [Read the coverage](#read-the-coverage)). Below it four tabs, each scrolling on its own: **Cameras** (the model for new cameras, **Place camera...**, the camera list and **Custom models**), **Camera** (the selected camera and where its footage lands), **Analysis** (the analysis settings and where every camera's footage lands) and **Places** (your areas and targets; see [Where footage lands](#where-footage-lands)). Selecting a camera anywhere opens the Camera tab; deselecting it there goes back to Cameras |
| Status bar | The active tool's keys and gestures as keycaps on the left (dropped from the end when the window is narrow); on the right the three coverage figures (street, 2+ cameras, walls), as at the top of the inspector, the OpenStreetMap attribution link and **OSM data as of** the date the map data was taken (hover it for the time, in UTC, and when the server built the area) |

## Pick an area

The demo street (a residential street in Juanita, Kirkland WA) is always one click away: **Demo street** in the top bar. It is built into the server and needs no map-data lookup, and it comes with ten example cameras of different makes and types, mounted on house fronts and facing the nearest street.

To load another street:

1. Click **Change area...**. A map dialog opens, centred on the current area.
2. Find your street. Paste a latitude and longitude into **Go to** and press **Enter** (or **Go**), and the map moves there at street level. Pan by dragging and zoom with the scroll wheel (or the **-** and **+** buttons).
3. Draw the area: turn on **Draw area** and drag a box, or hold **Shift** and drag. Or click **Square here** for a 300 m square at the map centre.
4. The box shows its size. Each side must be between **20 m and 500 m**; otherwise the box turns red and the reason appears next to the button.
5. Click **Load area**. Close the dialog with **Close**, **Esc**, or by clicking outside it.

**Go to** reads coordinates as map apps copy them: `51.5007, -0.1246`, `51.5007 -0.1246`, with degree signs (`51.5007°, -0.1246°`), with N, S, E and W (`51.5007° N, 0.1246° W`, in either order), in degrees, minutes and seconds (`51°30'02.5"N 0°07'28.6"W`), or as a `geo:` link. The latitude comes first unless N/S/E/W say otherwise. In Google Maps, right-click a place and click the numbers at the top of the menu to copy them; on a phone, long-press to drop a pin and copy its coordinates. Use a point for decimals: a pair written with decimal commas, a latitude outside -90 to 90 (often a sign that the longitude came first), a longitude outside -180 to 180, or a place further north or south than the map reaches (85°) is refused, and the message says why. Coordinates are read in the browser and nothing is sent anywhere.

If the server's operator has turned on place search, **Go to** also takes a place name or address, and the button reads **Search**. The search runs only when you press **Enter** or **Search**, never while you type. Up to five matches appear under the field, with the credit for the search data; click one to move the map there. The name you type goes to the server, which asks a geocoding service (usually OpenStreetMap's Nominatim) on your behalf; your browser never contacts that service. Repeated searches are answered from the server's cache, and the server limits how many new searches one address can make in a short time. Without place search, typing a name shows a reminder to use coordinates.

While the server fetches and prepares the data, a card in the 3D view says what is loading and for how long. A first request for a new place waits on the public map-data service and can be slow; after a few seconds the card says so, and offers **Use demo street instead**. **Cancel** stops waiting (the request is ignored when it arrives); it is not offered while reloading the area of a restored or imported scene, or after undoing an area change, because the cameras already belong to that area. Repeat requests for the same box are served from the server's cache. If loading fails, for example with "the map provider is busy; try again in about 30 s" when the map-data provider is busy, the message stays at the bottom of the view until you dismiss it, with a **Retry** button. Failed imports and restores also stay until dismissed; smaller problems, such as an invalid region, disappear after a few seconds.

### Local map data

If the server runs the region agent, the top bar has a database button. It opens **Local map data**, which lists the regions (Geofabrik extracts such as a city, state or country) that the server keeps its own copy of. Areas inside a ready region load from that copy in well under a second; everywhere else still uses the public service. The map dialog outlines these regions as **Local data**.

To add a region you need the server's admin token. Type it into the dialog (it is kept for this browser session only), search for a place, and click **Import**. Each match shows its download size, and extracts too large for the server say so instead of offering **Import**. The import runs on the server: it downloads the extract, keeps only buildings and roads, and builds a database. The dialog shows each step with a progress bar, and the top bar shows the running import (for example *Monaco 42%*), so you can keep working. A message appears when the region is ready, or why it failed. The update button re-imports the latest data, **Cancel** stops a running or queued import (the old data stays in use), and the bin deletes a region after you confirm. Each region shows how long ago it was downloaded, the date of its OpenStreetMap data and its size on disk, and above the list is the total. The server also refreshes regions on its own every week by default: each region has a **Refresh every** checkbox (unticked, the region keeps its data until you update it by hand) with the number of days next to it, which you can drag or click and type (7 to 365 days unless the server sets another range); **Default** returns a region to the server's interval. Each region says when its next refresh is due. Without the token, the dialog shows the regions read-only.

Loading a new area, including **Demo street**, removes the cameras you placed, because cameras belong to an area (the demo street then gets its ten example cameras again). A message with an **Undo** button appears, and **Undo** (or Ctrl+Z) brings the previous area and its cameras back. Undo only lasts for this visit, and autosave overwrites the stored scene within a couple of seconds, so to plan a second site without losing the first, start a **New scene** under **Scenes** first (or **Export** a copy).

## Place cameras

1. In the inspector's **Cameras** tab, choose a model in **New camera model**. Models are grouped by type (Doorbell, Floodlight, Spotlight, Bullet, Turret, Dome, Indoor) and show their horizontal field of view. Type in the search box at the top of the list to filter by maker, name or type (every word must match); **Enter** picks the first match of what you typed. See the [camera catalogue](camera-catalog.md) for sources. The default is a generic 160 degree doorbell.
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

Select a camera (in the viewport or by clicking its row in the list) to open the inspector's **Camera** tab, named after the camera. At the top, **Cameras** goes back to the list and the arrows step to the previous and next camera in list order (*2 of 10*). It goes on with the camera's colour, its type icon and its name (free text, up to 48 characters, edited in place), then:

| Field | Meaning |
| --- | --- |
| Model | Change the model without moving the camera (searchable, as above) |
| Colour | Colour of the camera body, its view cone and its per-camera overlay. Pick any colour or one of the quick swatches; **Default** returns to the colour for the camera's type |
| Group | Which group the camera is listed under (see below). Empty means by address: the field shows, greyed out, the address OpenStreetMap gives for the camera's position. Type a name (up to 80 characters) to group it yourself, or use the pin button to pick another address near your cameras or go back to **By address** |
| Position | **E** east, **S** south and **H** height above ground, in metres from the area centre. Height is limited to 0-60 m |
| Orientation | A compass dial: click or drag it to point the camera (Shift snaps to 15 degree steps); the shaded wedge is the horizontal field of view. Beside it, **Heading** (0-360 degrees clockwise from north, with a compass label such as NE; hover the label for the unit vector the camera faces as east, north, up), **Tilt** (-90 to 90 degrees; negative looks down) and **Roll** (-180 to 180 degrees) |
| Optics (collapsed) | Read-only. **Field of view**, **Resolution** and **Night vision** from the model (night vision shows "7.5 m (assumed)" in a warning colour when the manufacturer publishes no figure), **Modelled range** for the current detail level and lighting (see below), and **Source**, a link to the manufacturer's spec page for catalogue models |

Every number field (here, in Custom models and Subject height) changes when you drag across it: hold **Shift** for finer steps or **Ctrl** (**Cmd** on a Mac) for coarser ones, or click it to type a value.

Buttons: **Aim (A)**, **Move (M)**, **Focus (F)** (frame the camera in the view), **Duplicate** (a copy one metre east, with a new name) and **Delete**.

In the **Cameras** list, each row shows an eye, the camera's colour, its type icon, its name and its model. Click a row to select the camera and open its settings in the Camera tab (also for the selected camera, to get back to its settings; **Esc** or a click on empty ground deselects). Hovering a row highlights that camera in the 3D view, and hovering a camera in the 3D view highlights its row. The eye leaves a camera out of the coverage without deleting it (a crossed-out eye; the row is greyed out and the camera is ignored by the statistics and overlays). Right-click a row for **Select**, **Focus**, **Aim**, **Move**, **Duplicate**, **Include in coverage** and **Delete**.

### Groups by address

The list groups cameras by street address, worked out from OpenStreetMap as you place and move them. A group of two or more cameras has a heading with its address and how many cameras it holds; a camera alone at its address has no heading and shows the address in small type under its name instead. Groups appear in the order of their first camera, with **No address** last. A camera belongs to the nearest address within 25 m:

- a building tagged with an address (a camera on its wall, or on a pole in its yard), or
- an address mapped as a point inside a building (an entrance, or one of a terrace's houses). A camera on that building takes the closest of its points, or
- an address point on its own.

A building's own address wins over points inside it. The address is the house number (or house name), the street (or place; where OpenStreetMap gives neither, the nearest named road within 100 m) and any unit, such as "10011 Northeast 120th Street". City and postcode are left out because they rarely differ within one area. The number always comes before the street, so in countries that write it after the street it reads the other way round. If nothing in the loaded area has an address, there are no headings. A camera's **Group** field overrides its address, for example to list a camera on a detached garage with the house, or to group by owner.

### Look up an address

The **Address** tool (**L**) shows the address under the pointer in a small card beside it. Pointing at a building (its roof or a wall) gives that building's own address: its tags, else an address point inside it, else an address point in its yard (one whose nearest building it is, as some imports place them). It never borrows a neighbour's, so a building with none says **No address here**. Pointing at a yard or the street gives the nearest address within 25 m, as for cameras. Click to select that address: every building at it is highlighted, and the mode pill says how many cameras are there. The group those cameras are listed under is shaded in the camera list (its address heading, or a hand-set group; a camera alone at the picked address gets a heading for it). With **Settings → Click jumps to the camera list** on (the default), the click also opens the Cameras tab and scrolls the list to that group. **Delete** or a click where there is no address clears the selection, as does leaving the tool. Dragging orbits and pans as in Select.

## Read the coverage

The **Coverage** summary at the top of the inspector updates as soon as you change anything, and stays in view whichever tab is open. With no cameras placed it shows a short note instead of the figures, and the Cameras tab says how to place the first one. Under the figures, a line such as *Detect · Day · 1.00 m high* shows the analysis settings behind them; click it to open the Analysis tab.

| Figure | Meaning |
| --- | --- |
| **of street seen** | Share of the street surface area that at least one enabled camera can see |
| **by 2+ cameras** | Share of the street surface seen by two or more cameras |
| **of walls seen** | Share of building wall area (every building in the area) that at least one camera can see |

Below the bar, the square metres behind the street figure are shown, plus "open ground seen": all visible ground that is not under a building, which includes the street.

**What the figures leave out.** Under the figures, a line reads *Up to: trees, hedges, fences and vehicles are not counted. 'Street' includes driveways and footpaths.* Hover it for the full list (the same points as [Limitations and accuracy](#limitations-and-accuracy)). Leaving out hedges, fences and vehicles can only overstate what a camera sees, so read the figures as "up to"; but inferred building heights and missing or out-of-date buildings err in both directions, so they are not a strict upper bound either. The text is kept in one place in the app (`observation_core::provenance`, with a version number that scene files record), so anything else that prints figures uses the same words.

**When the map data was taken.** The last line of the summary, **OSM data as of 2026-10-31**, is the date of the OpenStreetMap data the area was built from (Overpass's `timestamp_osm_base`; for a [local region](#local-map-data), the date of its extract). Changes mapped since then are not in it. Hover it for the time in UTC and when the server built the area; the status bar shows the same date. If the data source did not say, the line reads **OSM data date unknown**. Quote the date with any figure you cite.

**Map data has changed.** A saved scene records which map data its figures came from (see `provenance` under [scene files](#undo-save-export-and-import)). When you restore or import a scene and its area comes back with different buildings, streets or addresses, a warning under the Coverage heading says *Map data has changed since this scene was saved (saved: X, now: Y)*, and a message appears briefly. The figures you see are for the current data, so they may differ from the ones the scene was saved with. Hover the warning for details (including when the scene was saved by another version of the app, which may read the same data differently); click **×** to dismiss it. It also goes when you load another area or open another scene. If the data date is the same as when the scene was saved, the map data did not change but this version of the app builds the area from it differently (building heights or street widths, say), and the warning says so instead. Edits to a road far beyond the area do not count. The check compares a fingerprint of the area's features, not the dates: the date of public OpenStreetMap data moves every minute, so a newer date alone does not mean anything in your area changed, and gives no warning.

**Limit the figures to an area.** To measure only part of the area, such as your own frontage or one block, open the **Limit** menu beside the Coverage heading and click **Draw area to limit to...**, then click the corners of the area on the ground (see [Draw and label areas](#draw-and-label-areas) for how drawing works). If you have drawn areas already, the menu also lists them: click one to limit the figures to it. The heading changes to "Coverage inside area", and every figure (street, walls, open ground) then counts only ground cells whose centre lies inside the outline, and wall patches whose ground half a metre in front lies inside it. The **Limit** menu, shown in the accent colour while a limit is on, then has **Only inside** (with the area's name and size), which turns the limit off and on without losing the outline (an area that is off is drawn faded, and also leaves the split described below), **Redraw**, and **Stop limiting**, which keeps the area under Places. The same choice is the **Limits the figures** box of an area in the Places tab. A scene saved with a region by an older version of the app opens with it as an unlabelled area that limits the figures, as before.

What counts as "street": every mapped road, service road (including driveways) and footpath, drawn as a band of its estimated width (about 14 m for a motorway down to 2 m for a footpath; the `width` and `lanes` tags are used when present), excluding anything under a building. Other way types, such as tracks, are ignored. Footpaths and service lanes therefore count towards the street figure.

What counts as "seen": a spot is seen when it is inside the camera's field of view, within the camera's modelled range, and a straight line from the camera to a person-height point above it does not pass through a building. Ground is sampled every 0.5 m and walls in roughly 1 m patches; a wall patch must also face the camera.

**Colours.** Switch with the **Overlap / Per camera** control under **Overlay colours** in the Analysis tab (or **N**):

- **Overlap:** a heat ramp by number of cameras that see a spot: 1 yellow, 2 orange, 3 red, 4 or more purple. The legend appears under the control.
- **Per camera:** each camera in its own colour (the swatch next to its name; see **Colour** above). Where views overlap, the camera lower in the list is painted on top.

## Where footage lands

The coverage figures answer a whole-street question. To ask about one camera and one place ("how much of this camera's view falls on next door?", "does it see my front door?"), draw areas and mark targets, then read the split and the verdicts. All of it is in the **Places** tab, the **Camera** tab and the **Analysis** tab.

The app reports what is visible in the model, not whether a camera is lawful. Areas and labels are your own statement about the ground ("as drawn"): no property boundaries or land registry data are used.

### Draw and label areas

Press **R** (the **Areas** tool), or **Draw area...** in the Places tab, and click the corners of the area on the ground. Clicking the first corner again (anywhere inside the circle drawn around it), or pressing **Enter**, closes the outline; **Backspace** removes the last corner and **Esc** cancels. Corners stay within the loaded area: while you draw, its edge is drawn in red and the map beyond it is tinted red and hatched, and a click out there puts the corner on the nearest point of the edge. The outline must not cross itself. The tool stays on after each area, so you can draw several in a row; the new area is picked in the Places tab.

Each area is listed in the Places tab with its colour, name, label, outline area (m²) and whether it limits the figures or is off. Click an area to pick it (its outline is drawn thicker in the 3D view) and open its controls:

- a **name** (for example *Front garden*; up to 60 characters),
- a **label**: **Own**, **Neighbour**, **Public**, **Other**, or **Unlabelled**,
- **Counts**: an area that is off is kept, drawn faded, and left out of the split and of any limit,
- **Limits the figures**: limit the Coverage figures to this area (one area at most),
- **Redraw**: click a new outline; the name, label and switches stay,
- **Delete**, with an **Undo** button in the message.

Outlines are drawn on the ground in a colour per label: green for Own, blue for Neighbour, light blue for Public, violet for Other and dark for unlabelled. At most 64 areas, each with at most 256 corners.

### Mark targets

A target is a named place to ask about: a front door, a side gate, a path. Press **T** (the **Target** tool), or **Mark target...** in the Places tab. Choose how it marks a target in the Places tab:

- **Click a wall or the ground** (the default): a click on a wall marks a 1 m square patch of it, centred on the click and facing the way the wall faces; a click on the ground marks a 1 m square there. A click on a roof marks nothing.
- **Outline on the ground**: click the corners of a small patch of ground and close it as for an area (at most 32 corners and 30 m across; draw an area for anything larger).

Targets are drawn in white: a ground target as its outline with a pin, a wall target as a cased square on the wall with a short line out from it. Each is named *Target 1*, *Target 2* and so on until you rename it in the Places tab, where it can also be deleted. At most 64 targets.

### Where a camera's footage lands

Select a camera and scroll down its Camera tab to **Where its footage lands**. At the current detail level, lighting and subject height (the line under the heading), it lists:

- **Ground seen**: all the ground the camera sees, m².
- Each **label** in use, with all its areas together (a spot inside two areas with the same label counts once), and under it each **area** with its outline's own area. Unlabelled areas follow.
- **Street, driveways and footpaths (as mapped)**: the street as mapped in OpenStreetMap, whatever areas are drawn over it. It holds roads, and also driveways (usually mapped as service ways) and footpaths, which are often private, so not all of it is public ground.
- **Unlabelled ground**: ground seen that is neither on that mapped street nor inside a labelled area.
- **Walls seen**, in total and on each building, nearest first, as *Building 12 · 14 m away*. Buildings are numbered as in the loaded area; the number can change when the area is built again. Addresses are not shown here: an address comes from the nearest address within 25 m, which is a guess, so the app never names a neighbour automatically.

Each area, label and unlabelled figure is a range, such as *30–41 m² (as drawn 36)*: the low figure has every outline moved 1 m inward, the high figure 1 m outward, and the figure in brackets is as drawn (for unlabelled ground it is the other way round, since moving the outlines out leaves less ground outside them). So an outline drawn a little too generously or too tightly cannot quietly give "0 m² of the neighbour's", and printing each outline's own area beside it shows an outline drawn far too large. Areas may overlap, so their figures need not add up.

The **Analysis** tab has the same split for every camera in coverage, side by side: ground, street, each label in use, unlabelled ground and walls, in m², each range with its as-drawn figure under it. Cameras overlap, so the rows do not add up.

### Target verdicts

For each target, the Places tab gives every camera's verdict at the current settings (click the target to see them all; its row says for how many cameras it is not ruled out). The Camera tab lists the selected camera's verdict on every target.

- **Not ruled out**: some of the target is visible in the model from that camera, within its range.
- **Ruled out**, with the reason: **outside the view** (no part of it is in the field of view), **out of range** (in view, but beyond the range for the detail level and lighting), **blocked by a building** (every part in view and range is hidden by a building, lies under one, or is on the far side of its wall), or **blocked by the terrain**.

The wording is one-sided on purpose. Hedges, fences, trees and vehicles are not modelled and can only block more, so "not ruled out" never means the camera sees it; "ruled out" is a firm answer within the model. Under each verdict a ladder shows it at every detail level under the current lighting, then at the other lighting at the current level, for example *Detect ✓ · Observe ✓ · Recognise ✓ · Identify ✗ · Night ✗* (✓ not ruled out, ✗ ruled out; the current settings are highlighted). Hover a step for its range and reason.

Every split and verdict ends with the caveats line and the date of the map data, as the Coverage card does; quote both with any figure you cite.

## Analysis settings

These are in the inspector's **Analysis** tab. They apply to every camera and are saved with the scene.

**Detail level** sets how many pixels cover one metre of a person, using the DORI levels (EN 62676-4):

| Level | Pixels per metre | Meaning |
| --- | --- | --- |
| Detect | 25 | Can tell someone is there (default) |
| Observe | 62 | Clothing and activity |
| Recognise | 125 | A person you know |
| Identify | 250 | A stranger, beyond doubt |
| Custom | 1-2000 (default 250) | Your own value, for calibrating against real footage |

**Custom** shows a **Pixels per metre** field so you can test any density. A higher density shrinks each camera's range. The range for a level is `width_px / (2 x px_per_m x tan(hfov / 2))`, capped at 100 m. For example a 1920 px wide, 90 degree camera reaches about 38 m at Detect, 15 m at Observe, 7.7 m at Recognise and 3.8 m at Identify.

**Lighting.** **Day** uses the DORI range only. **Night (IR)** uses the smaller of the DORI range and the camera's night-vision range. If the manufacturer publishes none, the app assumes **7.5 m** (about 25 ft, typical for consumer infrared LEDs) and flags it in the camera's details. The cameras' own floodlights and street lighting are not modelled.

**Subject height** is the height above ground of the point that has to be visible. Presets: **Ground** (0 m), **Torso** (1.0 m, the default) and **Face** (1.6 m); or type a value from 0 to 2.5 m.

## Custom models

Open **Custom models** (at the bottom of the inspector's Cameras tab) to add a camera that is not in the catalogue. Fill in name, type, horizontal and vertical field of view (10-180 degrees), resolution (width 160-8192, height 120-8192) and night-vision range (0-60 m; 0 means no night coverage), then click **Add model**. The model is selected for new cameras, appears in the model lists, and is stored with the scene, and the form goes on to edit it (pick **New model** under **Model** to add another). A scene holds at most 64 custom models.

To change one of your models, pick it under **Model** at the top of the form (it shows **New model** while you are adding one). The fields fill in with its values, unchanged even where they lie outside the ranges the fields offer (as an imported model's may); change them and click **Save changes**. The model keeps its identity, so cameras that already use it pick up the new values. **Delete** removes it, with an **Undo** button in the message. A model that cameras still use cannot be deleted: the message says how many use it, so give them another model or delete them first. If the deleted model was the one for new cameras, new cameras go back to the default model. Catalogue models cannot be changed.

## Undo, save, export and import

- **Undo and redo.** The arrows in the top bar, or **Ctrl+Z** and **Ctrl+Shift+Z** (or **Ctrl+Y**; **Cmd** on a Mac), step back and forward through your changes: placing, moving, aiming, editing and deleting cameras, areas and targets, analysis settings, custom models and area changes. A drag or a typed name counts as one step when you let go or leave the field. Hover an arrow to see what it would undo or redo. Deleting a camera, an area, a target or a custom model, or replacing the cameras, also shows a message with an **Undo** button. Undo history is kept for this visit only (up to 100 steps) and for the open scene only: opening, starting or importing another scene clears it (a **Duplicate** keeps it). Undoing an area change reloads that area. While a text field has focus, Ctrl+Z undoes typing in the field instead.
- **Autosave.** The open scene (area, cameras, custom models, analysis settings, areas and targets) is saved to your browser's local storage about every 1.5 seconds. The scene open last is restored the next time you open the app in the same browser. Clearing site data, private windows and some browser settings discard it. The status bar shows **Saved** once a save has gone through, or **Not saved** in a warning colour if the browser refused to store the scene; hover it to see why (storage full, site data blocked or no storage, as in some private modes), then use **Export** to keep your work.
- **Scenes.** A browser keeps several scenes, for example one per site. **Scenes** in the top bar lists them with their sizes: click one to open it (the open one is saved first), **New scene** starts an empty one on the current area, **Duplicate** copies the open one and carries on in the copy, the pencil renames a scene and the bin deletes one after you confirm (deleting the open scene opens the next). Scenes cannot be switched, and the open one cannot be deleted, while an area is loading. Below the list is how much storage the scenes take, out of the 5 MB or so most browsers keep for a site; when it is full, scenes stop saving until you delete one. A scene saved before scenes existed is the first one. If the scene open last cannot be read, it is kept as it was and the demo street opens as a new scene.
- **Export** downloads the open scene as a JSON file named after it. **Import** adds a chosen JSON file as a new scene named after the file and opens it, leaving the open scene as it was, and reloads its area from the server (a file picked while an area is loading waits for it). In a browser that offers no storage or blocks it, Import replaces the open scene instead, and can be undone.

Scene files are plain JSON:

| Field | Notes |
| --- | --- |
| `format` | Must be `"observation-scene"` |
| `version` | `2` (`1`: cameras on flat ground, raised onto the terrain when the area loads). Files with a newer version are refused |
| `area` | `{"kind": "demo"}` or `{"kind": "osm", "bbox": {"south", "west", "north", "east"}}` |
| `origin` | `{"lat", "lon"}` of the area centre that camera positions are relative to |
| `cameras` | Optional list. Each has `id`, `label`, `model_id`, `position` as `[east, up, south]` in metres, `yaw_deg` (compass bearing), `pitch_deg`, `roll_deg`, `enabled`, and optionally `color` as `[r, g, b]` (0-255; absent means the default for the model's type). The `color_index` field of older files is ignored |
| `custom_models` | Optional list of models: `id` (unique, starting with `custom-`; see below), `vendor`, `name`, `kind`, `hfov_deg`, `vfov_deg`, `resolution` as `[w, h]`, and optionally `ir_range_m`, `source`, `notes` |
| `params` | Optional: `dori` (`Detect`, `Observe`, `Recognise`, `Identify`, `Custom`), `custom_ppm` (1-2000, default 250; used with `Custom`), `lighting` (`Day`, `Night`), `subject_height_m` |
| `areas` | Optional list of drawn areas: `id`, optionally `label` (`own`, `neighbour`, `public` or `other`; absent means unlabelled) and `name`, `points` (corners as `[east, south]` in metres, at most 256) and `enabled`. At most 64 |
| `limit_figures_to` | Optional: the `id` of the area the coverage figures are limited to |
| `targets` | Optional list of named places: `id`, `name`, and `at`, either `{"kind": "ground", "points": [...]}` (at most 32 corners and 30 m across) or `{"kind": "wall", "point": [east, up, south], "normal": [east, south]}` (the direction the wall faces). At most 64 |
| `region` | Written as a copy of the area named by `limit_figures_to`, for older versions of the app. A file from an older version has a `region` (`points` and `enabled`) and no `areas`: it loads as one unlabelled area that limits the figures, as before |
| `provenance` | Optional, written once the area has loaded: `osm_data_as_of` (when the OSM data was current, such as `"2026-10-31T19:43:02Z"`, UTC; absent if unknown), `bundle_built_at` (when the server built the area, UTC), `osm_fingerprint` (16 hex digits identifying the area's map features), `app_version`, `catalog_version` (16 hex digits identifying the camera catalogue's models) and `caveats_version` (which statement of what the figures leave out they were shown with). Older files without it load as before, and older versions of the app ignore it |

Imports are checked: at most 256 cameras and 64 custom models, finite positions and angles within 10,000, subject height 0-10 m, custom pixel density 1-2000 px/m, and valid custom models. A malformed `provenance` block, or a time in it that is not a UTC timestamp, is dropped rather than failing the import. A custom model whose `id` repeats an earlier one or does not start with `custom-` is given a new `custom-` id (cameras that used it follow it). A spec-sheet `source` link that is not a plain `https://` URL is removed from the model, and an area with more than 256 corners or corners beyond 10,000 m, or a target that is out of range or too large, is removed from the scene, rather than failing the import. Area and target names are trimmed and cut to 60 characters. A camera, area or target whose `id` repeats an earlier one's, or is above 2,147,483,647, is given a new id. A bad file shows an error toast and adds no scene.

## Export an evidence sheet

**Sheet…** in the top bar makes a dated sheet of the figures that someone without the app can read: in an article, an agenda packet or a conversation with a neighbour. It states its own settings, date and limits:

- **Provenance and settings**: when it was made, the app version, the date of the OpenStreetMap data and when the area was built, the map data's fingerprint, the camera catalogue and caveats versions, the detail level and lighting shown, the subject height and the area the figures are limited to.
- **Cameras**: each enabled camera by number, with its model and its range by day and at night at the level shown.
- **Detail × lighting**: the street seen by at least one camera and by two or more, and the walls seen, at every detail level by day and at night, each figure with the area it is out of (*359 of 1,651 m², 22%*) and a plain reading of the level (from *can tell someone is there* to *can identify a stranger, beyond doubt*). There is no single headline number. Below it, the street at the level shown is split into carriageway and footpath and service ways.
- **Where footage lands**: each camera's split as in the Analysis tab, as drawn, with ranges, and each area with its outline's area.
- **Targets**: for each target, which cameras do not rule it out at the settings shown, how many do and why, and each camera's ladder by day and at night, in the same one-sided wording as the Places tab.
- **Methodology**: what "street" and "seen" mean (the road types and their widths, the detail levels and the range formula, written from the values the app computes with), how areas, targets and verdicts are worded, and the full, versioned list of what the figures leave out, with why they read as "up to", and any changes an AI agent proposed and you accepted (see [Review a proposal](#review-a-proposal)), each with the time you accepted it. The Exact sheet gives each change's summary as the agent wrote it; the Aggregate sheet gives only how many there were and when you accepted them, because a summary is the agent's own words and may name a camera's position, its label or an address.

Every page says it is an estimate from a model, not recorded footage, and carries the scene name, the date, the app version, the data date and fingerprint, the map data's credit (© OpenStreetMap contributors, ODbL) and the elevation data's credit.

Choose what the sheet shows:

- **Aggregate** (the default) has the numbers only: no map, no coverage shapes, no camera positions or labels, no addresses, and no distance from a camera to a building or a target. This is the one to share. The shape of what a camera sees gives its position away (the edges of its view meet where it is), and a distance from a camera to a numbered building does too, so they are left out. Area, target and scene names are printed as you typed them, so keep addresses out of them if the sheet is to be shared.
- **Exact** adds a map of the area with every camera's position and direction, the coverage at the level shown (by how many cameras see each spot), the areas and targets, a scale bar and a north arrow, plus camera labels and the walls each camera sees per building. It is watermarked *shows camera positions*, inside the map and across every page: keep it for your own records, or share it only with the agreement of everyone whose camera is on it.

Then save it:

- **Download SVG**: a vector image that opens in any browser.
- **Download PNG**: a picture at twice the page size, drawn by your browser.
- **Print or save as PDF…**: your browser's print dialog, one A4 page per page of the sheet; choose *Save as PDF* there for a PDF.

The files are named after the scene, the date and the tier, for example `Front of house evidence 2026-11-14 (aggregate).svg`. The sheet uses the coverage already worked out, so the buttons wait until every camera's coverage is ready. Nothing is sent anywhere: the sheet is made in your browser.

## Connect an AI agent

An AI agent running on your computer, through the Observation MCP server (see the [README](../README.md#ai-agents)), can read what you have open: the scene, its area, the selected camera and a point you share. It cannot change your scene by itself: it can propose a change, which you accept or reject (see [Review a proposal](#review-a-proposal)), and it can move your view, select and mark things, and ask you to load an area (see [Let the agent show you things](#let-the-agent-show-you-things)). Coverage and address questions then work on your real area, not only the demo street.

The server offers this only when it runs with `OBS_AGENT_BRIDGE=on` and you open the app on the same computer (at `http://localhost:...` or `http://127.0.0.1:...`); otherwise there is no **Connect agent** button.

1. Click **Connect agent** in the top bar, then **Connect**. The dialog shows a code such as `ABCD-EFGH`.
2. Ask your agent to connect to the Observation app with that code and the address the app is open at (for example *"connect to Observation at http://127.0.0.1:8080 with code ABCD-EFGH"*). It calls `connect_app`. The code works once and expires after 10 minutes; a few wrong tries withdraw it, and the dialog says so.
3. Once connected, the top bar shows **Agent connected**. The agent sees your changes within about a second. Point at the ground and press **S** to share that spot ("why can't this camera see here?").

The connection ends when you click **Disconnect** (in the dialog), when the agent disconnects, when you close the tab, or after 30 minutes in which neither side used it. Nothing about it is stored on the server, and a reload starts afresh.

A small model running on your own computer, such as Qwen3 4B in llama.cpp's chat page, works better with fewer tools: start the MCP server with `OBS_MCP_PROFILE=small` to work offline on the demo street, or `OBS_MCP_PROFILE=small-app` to work in your open app (connect, propose a change, wait for your decision and show you the result). These profiles offer about 16 to 23 tools instead of about 60, with plainer descriptions and shorter answers; the default, `full`, offers every tool. `OBS_MCP_TOOLSETS` picks groups of tools instead (see [Small models and tool profiles](design.md#small-models-and-tool-profiles)). Ask the agent to call `get_started` first; the prompt `drive_the_app` walks it through working in your app step by step.

Offline or connected, the agent can mark the places you talk about: label ground you describe as your own, a neighbour's, public or other (`label_area`; as you describe it, not from property records) and name targets such as your front door or a side gate (`add_target`, on a patch of ground or a building wall). It can then say where each camera's footage lands (`camera_split`: square metres of visible ground in each area, on the street, driveways and footpaths as mapped (not all of it public) and elsewhere, as a range with the outlines moved 1 m in and out, and the wall seen per building), whether the model rules out that a camera sees each target or a point, and why (`target_verdicts`, `check_point`), and which stretches of street no camera sees (`find_gaps`). Verdicts are one-sided: *not ruled out* or *ruled out* with the reason, never *seen*, since hedges, fences, trees and vehicles are not modelled and can only block more. Buildings are named by number and distance, never by a neighbour's address. Every answer carries the map data's date and the caveats. The prompt `where_footage_lands` walks an agent through it.

An agent can also get a sense of the scene before it changes anything: a summary of the whole scene (`scene_summary`), what is around a point (`describe_surroundings`), the buildings and streets with their sizes and directions (`list_buildings`, `list_streets`), what lies in one camera's view and what blocks it (`camera_view`), wall mounts facing the street for a new camera (`suggest_mount_points`), and conversion between latitude and longitude and the scene's positions (`convert_position`).
To get its bearings, the agent can draw the scene from above (`render_map`): a north-up map in characters, with buildings, streets, the ground some camera is not ruled out from seeing, the cameras, targets, your areas and the point you shared, and a legend; agents that read images can ask for a picture of the same map as well. Buildings carry no addresses on it.

### Review a proposal

When the agent proposes a change, a message says so and a **proposal card** opens at the top left of the 3D view. It shows:

- the agent's summary, and a line for every change it would make (cameras added, moved, aimed, changed or removed, custom models, areas, targets, analysis settings);
- the street seen now and after accepting, at the selected detail level and lighting ("computing…" while the new cameras' views are worked out), with the map data date and what the figures leave out;
- in the 3D view, a **ghost** of the proposal: new and moved cameras drawn translucent with their view cones (a moved one with a line from where it stands now), removed cameras crossed out, and new or changed areas and targets outlined.

**Accept** applies every change at once; there is no partial acceptance, so the figures you saw are the ones you get. Your own edits to other cameras stay as they are. The whole proposal is one undo step, named "Agent: " and the summary, and **Undo** in the message after accepting reverts it. Accepted proposals are recorded in the scene's change log (their summary and when you accepted them), which is saved and exported with the scene. **Reject** leaves the scene as it is; anything you type in the reason box goes to the agent.

If you change a camera, area, target or setting that the proposal touches before you decide, the card says it is **stale** and Accept is disabled: the proposal no longer fits your scene, and it is never adjusted to fit. The agent is told, and can propose again from your current scene. A newer proposal from the agent replaces the one on the card, and one the agent withdraws, or that is left when the connection ends, can no longer be accepted. Closing the card keeps an undecided proposal waiting: **Proposal waiting** in the top bar opens it again.

Everything on the card comes from the agent: read the change lines and look at the ghost, not only the summary.

### Let the agent show you things

While connected, the agent can point you at what it is talking about. It never changes your scene this way, and a message at the bottom of the view says each time what it did ("Agent moved the view", "Agent selected camera 3 ‘Porch’"):

- **move the view** to look at a spot or a camera (`focus_view`);
- **select a camera** (`select_cameras`; the app selects one camera at a time);
- **mark** a camera, a target, an area or a spot, with a short note beside the mark (`highlight`). A new mark replaces the last; the **×** on the note removes it;
- **change how coverage is shown** (`set_view_options`): the overlay colours, and the **Cones** and **Walls** toggles. Day or night and the detail level are part of your scene, so the agent cannot switch them this way: it proposes them, as any other change to the scene.

Untick **Let the agent move the view and selection** in the Connect agent dialog to stop all of these; the app then tells you once that the agent tried. The choice is remembered in this browser.

The mark, and a request to load an area you have not answered, go when you open another scene or load another area. The agent can also **ask you to load an area**, for example after searching for your street (`search_places`, which works only when the server's operator has turned on place search). A card says "The agent asks to load an area … because …", with **Load** and **Dismiss**. Nothing loads unless you click **Load**, and loading it replaces the area open now, as choosing it on the map does. The card appears even with the box above unticked.

## Keyboard, mouse and touch

| Input | Action |
| --- | --- |
| **V** / **P** / **A** / **M** / **R** / **T** / **L** | Select / Place / Aim / Move / Areas / Target / Address tool |
| **Enter**, **Backspace** (Areas tool, or the Target tool's outline) | Close the outline; remove the last corner |
| Left-drag | Orbit the view (with Aim or Move and a camera selected: aim or move it) |
| Right-drag, or **Shift** + left-drag | Pan (Shift-drag does not pan while Aim or Move is driving a camera; use right-drag) |
| Scroll wheel | Zoom (8 m to 1200 m from the focus point) |
| Click | Select a camera, or use the active tool |
| **F** | Focus the view on the selected camera (moving in to within 60 m if you were farther away) |
| **N** | Toggle Overlap / Per camera colours |
| **S** | With an agent connected: share the point of the ground under the pointer with it |
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
- **Buildings and the lie of the land.** OpenStreetMap buildings and the ground block the view. Trees, hedges, fences, vehicles, poles, awnings, bridges and tunnels are not modelled. Buildings are flat-roofed boxes, so roof pitch and dormers are ignored, and `building:part` outlines are not loaded separately.
- **Terrain is approximate.** Ground heights come from a public elevation model (about 10 m resolution in the US, 30 m in much of the world), sampled every 5 m: small banks, retaining walls and cuttings are smoothed out. Each building stands on the lowest ground under it, so on a slope its uphill side runs into the ground. Cameras placed on the ground stand on it at their mount height; scenes saved before terrain are raised onto it when they load.
- **Heights are inferred.** Use the `height` tag when present (units such as `m`, `ft` and `12'6"` are understood); otherwise `building:levels` at 3 m per storey (plus 1 m for the roof when no roof levels are tagged); otherwise a default for the building type: sheds, garages, carports and cabins 3 m, bungalows 4 m, houses and residential buildings 6.5 m, apartments, dormitories and hotels 10 m, churches 12 m, anything else 6 m. Heights are limited to 1-300 m. Many areas have few height tags, so wall figures in particular can be off.
- **Map data varies, and ages.** The figures are for the OpenStreetMap data of the date shown in the Coverage card and the status bar. OpenStreetMap coverage and accuracy differ by place; missing buildings let cameras see through the gap, and missing addresses leave cameras under **No address** (or under a neighbour's address) until you set their group.
- **Night.** The 7.5 m assumption applies only when no night-vision range is published; floodlights and street lights are ignored.
- **One area at a time,** 20-500 m per side. Only the loaded area is analysed, so views beyond its edge are cut off.
- **Single person-height sample.** A spot is judged by one point at the subject height; people of other heights or crouching behind low cover are not distinguished.
- **Browser needs.** WebAssembly and WebGL2 are required. The light or dark theme follows your system setting, including when it changes while the page is open.
