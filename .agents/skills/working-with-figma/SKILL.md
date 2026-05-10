---
name: working-with-figma
description: Read, inspect, and modify Figma designs programmatically. Use when the user mentions Figma files, designs, UI mockups, or wants to create/update design elements. Covers reading design context, extracting component specs, and writing back to Figma via the Plugin API (figma_use_figma).
metadata:
  author: my-sanctuary
  version: '1.0'
---

# Working with Figma

This skill documents how to interact with Figma designs programmatically using the MCP tools.

## File & Node Identification

Given a Figma URL like:

```
https://www.figma.com/design/C3bNW27a5s70duoTXxP8Me/Fahim-s-Sanctuary?node-id=0-1
```

- `fileKey` = `C3bNW27a5s70duoTXxP8Me` (path segment after `/design/`)
- `nodeId` = `0:1` (replace `-` with `:` in the `node-id` query param)

## Reading Design Context

### get_metadata — Structure overview

Returns XML of all nodes on a page. Use to discover layer names, IDs, positions, sizes.

```xml
<canvas id="0:1" name="Page 1">
  <frame id="1:1625" name="Home" x="-720" y="-264" width="1440" height="973">
    <frame id="1:1626" name="Social Media App Header" ...>
```

### get_design_context — Implementation-ready code

Returns React+Tailwind code, image URLs, and style tokens for a node. Use when implementing a design in code.

Returns:

- Component code (React + Tailwind)
- Image/icon asset URLs
- Font names and weights (`Sen:Bold`, `Kumbh Sans:Medium`, `Cutive:Regular`)
- Color values in hex and rgba

### get_screenshot — Visual reference

Returns a URL to a PNG screenshot of a node.

### search_design_system — Find components/variables

Search for design tokens, components, and styles by keyword.

## Writing to Figma (figma_use_figma)

Executes JavaScript via the Figma Plugin API to modify the file.

### Known Limitations (MCP Sandbox)

1. **Parent/child**: Set parent via `parentNode.appendChild(childNode)`. The `.parent =` assignment does NOT work.
2. **Effects**: `DROP_SHADOW` and similar effect types are NOT supported. Only `LAYER_BLUR` and `BACKGROUND_BLUR` are valid. Skip effects entirely or use none.
3. **Font loading**: Must use top-level `await` — `async function` wrappers and `.then()` callbacks do NOT persist mutations.

   ```js
   // ✅ WORKS
   await figma.loadFontAsync({ family: "Inter", style: "Bold" });
   // ... create text nodes ...

   // ❌ DOES NOT PERSIST
   async function main() { await loadFonts(); /* ... */ }
   main();

   // ❌ DOES NOT PERSIST
   figma.loadFontAsync({...}).then(() => { /* ... */ });
   ```

4. **Position & layout**: Setting `node.x` / `node.y` may not persist if the parent frame has an auto layout mode set. Always set `parent.layoutMode = "NONE"` on container frames before positioning children.
   ```js
   parent.layoutMode = 'NONE'; // Required for absolute positioning
   child.x = 952;
   child.y = 288;
   parent.appendChild(child);
   ```
5. **Text creation flow**: Load fonts BEFORE setting any fontName or characters. Set `fontName` BEFORE `characters`.
   ```js
   await figma.loadFontAsync({ family: 'Inter', style: 'Bold' });
   const t = figma.createText();
   t.fontName = { family: 'Inter', style: 'Bold' }; // Set first
   t.characters = 'Hello'; // Then set
   ```
6. **Default font**: Newly created text nodes default to `"Inter Regular"`. The actual fonts used in the design may differ (check errors or design context). When using `figma.createText()` directly (not cloning), the font must be loaded even if it's the default.
7. **Clone limitaion**: `sourceText.clone()` copies the font reference but the font still must be loaded before modifying characters.

## Sanctuary Design Language

When designing for this project, use these tokens:

| Token          | Value                    | Usage                                    |
| -------------- | ------------------------ | ---------------------------------------- |
| Background     | `#f5f5ef`                | Page/section backgrounds                 |
| Primary        | `#204f0a`                | Buttons, active states, headings         |
| Text primary   | `#1d221b`                | Body text                                |
| Text muted     | `rgba(29,34,27,0.62)`    | Secondary text                           |
| Text faint     | `rgba(29,34,27,0.4)`     | Placeholder text                         |
| Border         | `rgba(23,52,9,0.16)`     | Dividers, outlines                       |
| White          | `#fbfffa` / `#ffffff`    | On-dark text, card backgrounds           |
| Font heading   | `Sen:Bold`               | Logo, headings                           |
| Font body      | `Kumbh Sans:Medium`      | Body text                                |
| Font body bold | `Kumbh Sans:SemiBold`    | Button text, labels                      |
| Font display   | `Cutive:Regular`         | Large display text (titles, day numbers) |
| Radius         | `22px` / `24px` / `28px` | Rounded corners (buttons, cards)         |

## Common Patterns

### Creating a frame with content

```js
const frame = figma.createFrame();
frame.name = 'My Frame';
frame.resize(width, height);
frame.x = 100;
frame.y = 100;
frame.fills = [{ type: 'SOLID', color: { r: 0.96, g: 0.96, b: 0.94 } }];
page.appendChild(frame);
```

### Adding a button

```js
const btn = figma.createFrame();
btn.resize(140, 44);
btn.fills = [{ type: 'SOLID', color: { r: 0.13, g: 0.31, b: 0.04 } }]; // #204f0a
btn.cornerRadius = 22;
parent.appendChild(btn);
```

### Adding text

```js
await figma.loadFontAsync({ family: 'Kumbh Sans', style: 'SemiBold' });
const t = figma.createText();
t.fontName = { family: 'Kumbh Sans', style: 'SemiBold' };
t.characters = 'Hello';
t.fontSize = 14;
t.fills = [{ type: 'SOLID', color: { r: 1, g: 1, b: 1 } }];
t.textAutoResize = 'WIDTH_AND_HEIGHT';
parent.appendChild(t);
```

### Positioning with auto-layout disabled

```js
container.layoutMode = 'NONE'; // Critical for absolute positioning
child.x = targetX;
child.y = targetY;
container.appendChild(child);
```

### Color values (0-1 range)

Convert hex to RGB 0-1:

```js
// #204f0a → { r: 0.13, g: 0.31, b: 0.04 }
// #f5f5ef → { r: 0.96, g: 0.96, b: 0.94 }
// With alpha: { r: 0.11, g: 0.13, b: 0.11, a: 0.62 }
```

### Looking up existing nodes

```js
// By name (scoped to a parent or page)
const button = page.findOne((n) => n.name === 'Sign in with Google');
// By type
const allText = page.findAll((n) => n.type === 'TEXT');
```

### Removing nodes

```js
const node = page.findOne((n) => n.name === 'Old Frame');
if (node) node.remove();
```

## Component Pattern: Navbar with Active States

The workspace has a master **Navbar** component (id `80:2`, at x=3000 y=0) with instances on all 4 screens. This is the canonical approach for shared UI in this project.

### Component Structure

```
Navbar (Component)
├── HeaderBg              — rect 1440×76, #fafffa
├── Logo                  — text "Sanctuary", Sen Bold 18, #214f0a at (32,26)
├── Nav-Home              — text, Kumbh Sans SemiBold 14 at (200,26)
├── Nav-Streams           — text, Kumbh Sans SemiBold 14 at (290,26)
├── Nav-Calendar          — text, Kumbh Sans SemiBold 14 at (380,26)
├── Nav-Settings          — text, Kumbh Sans SemiBold 14 at (470,26)
├── Underline-Home        — rect 56×2 centered under Home text (x≈188), hidden
├── Underline-Streams     — rect 56×2 centered under Streams text (x≈290), hidden
├── Underline-Calendar    — rect 56×2 centered under Calendar text (x≈376), hidden
├── Underline-Settings    — rect 56×2 centered under Settings text (x≈467), hidden
├── SearchBg              — rect 240×36 at (720,20), rounded 18
├── SearchText            — text "Search..." at (736,28)
├── BellBg                — rect 20×20 at (1060,28), rounded 10
├── NewPostBtn            — rect 130×36 at (1140,20), rounded 22
├── NewPostText           — text "+ New Post" at (1170,28)
└── HeaderDivider         — line 1440×0 at (0,76)
```

### Active State Pattern

Each nav item has a dedicated underline layer that is **hidden by default**. To set the active state on an instance, toggle visibility of the relevant underline and set the nav text fill to the active color:

| Instance         | Active Nav | Underline Shown    |
| ---------------- | ---------- | ------------------ |
| Home (80:19)     | Home       | Underline-Home     |
| Streams (80:36)  | Streams    | Underline-Streams  |
| Calendar (80:53) | Calendar   | Underline-Calendar |
| Settings (80:70) | Settings   | Underline-Settings |

### Creating a Component via Plugin API

```js
const comp = figma.createComponent();
comp.name = 'MyComponent';
comp.resize(width, height);
comp.layoutMode = 'NONE';

// Add child elements
comp.appendChild(childNode);

page.appendChild(comp); // Place on canvas
```

### Creating and Configuring Instances

```js
// Create instances from a component
const inst = component.createInstance();
inst.name = 'MyInstance';
inst.x = 0;
inst.y = 0;

// Override layer properties on the instance
const underline = inst.findOne((c) => c.name === 'Underline-Streams');
underline.visible = true;

const navText = inst.findOne((c) => c.name === 'Nav-Streams');
navText.fills = [
  { type: 'SOLID', color: { r: 33 / 255, g: 79 / 255, b: 10 / 255 } },
];
// activeFill = #214f0a

targetFrame.appendChild(inst);
```

**Important**: `instance.findOne()` searches within the instance's overridable layers. Supports `visible`, `fills`, and `characters` overrides.

### Navbar Positions (Unified)

| Element                         | x      | y   | w    | h   |
| ------------------------------- | ------ | --- | ---- | --- |
| HeaderBg                        | 0      | 0   | 1440 | 76  |
| Logo                            | 32     | 26  | —    | —   |
| Nav-Home                        | 200    | 26  | —    | —   |
| Nav-Streams                     | 310    | 26  | —    | —   |
| Nav-Calendar                    | 420    | 26  | —    | —   |
| Nav-Settings                    | 530    | 26  | —    | —   |
| Underline (centered under text) | varies | 52  | 56   | 2   |
| SearchBg                        | 720    | 20  | 240  | 36  |
| BellBg                          | 1060   | 28  | 20   | 20  |
| NewPostBtn                      | 1140   | 20  | 130  | 36  |
| HeaderDivider                   | 0      | 76  | 1440 | 1   |

## Canvas Layout

All screens are on Page 1 with these positions:

| Screen           | Frame ID | x    | y    | w    | h    |
| ---------------- | -------- | ---- | ---- | ---- | ---- |
| Login            | 36:2     | 769  | -264 | 1440 | 1024 |
| Home             | 1:1625   | -720 | -264 | 1440 | 1393 |
| Streams          | 56:2     | -720 | 1200 | 1440 | 1024 |
| Calendar         | 58:2     | 769  | 1200 | 1440 | 1024 |
| Settings         | 60:2     | -720 | 2304 | 1440 | 2408 |
| Navbar Component | 80:2     | 3000 | 0    | 1440 | 77   |

## Edge Cases

1. **Overlapping frames**: When adding new screens alongside existing ones, calculate positions carefully. Use get_metadata to find existing frame positions and place new frames beside them (e.g. `newX = existingX + existingWidth + 48`).
2. **Node IDs**: IDs like `0:1` represent pages/canvases. Created nodes get auto-incrementing IDs (`1:1625`, `45:2`, etc.).
3. **Page names**: The default page is named "Page 1" (id `0:1`). Page creation via `figma.createPage()` does NOT work in this environment — create all content on the existing page.
4. **Image assets**: The `get_design_context` tool returns URLs for images/vectors. These URLs expire after ~7 days.
