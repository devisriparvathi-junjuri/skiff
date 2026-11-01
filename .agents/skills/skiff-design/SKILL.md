---
name: skiff-design
description: Skiff's design rules for UI, interaction and copy. Use before you add or change anything under apps/desktop/src that the user sees or presses (CSS, menus, dialogs, keys, labels, themes), and when you review such a change.
---

# Skiff design

The published copy is the wiki [Design](https://github.com/solutionscay/skiff/wiki/Design) page. Change both together.

Where the rules live in code: tokens in `apps/desktop/src/styles/tokens.css`, keys in `app/keys.ts` (`keyLabel`, `DESCRIBE`), actions in `app/actions.ts`, menus in `ui/menu.ts` and `workspace/menus.ts`, theme pickers in `appearance/themes.ts`.

How Skiff looks and behaves. Read this before you change the UI. Terms follow the [Glossary](https://github.com/solutionscay/skiff/wiki/Glossary).

## The essence

Skiff is a control surface for many long-running agent sessions. It shows where each session is and what it needs. The apps the user already has do everything else.

Test every change with three questions:

1. Does it make many sessions easier to manage?
2. Does the user always know where the keys are and what an action changes?
3. Does Skiff stay small?

## Product rules

- **Panes are for sessions.** Files and diffs open as Settings › Open with says. A terminal tool runs in the peek over the terminals. An app opens its own window. A file with no command goes to the system's default app. The peek never adds a session pane, and its tool is not a session to the user.
- **The current row owns the main area.** A change, file or folder row shows its preview. A session or group row shows its panes. Only `select()` changes the current row; a render only ends a preview. Enter goes into what shows. Space opens or closes.
- **Sessions outlive the window.** `skiffd` owns every PTY. Closing a pane, a group or the window does not end a session. End and Kill ask first. Closing or removing a project also ends its sessions. Close asks when sessions run; Remove always asks.
- **Hands off the agent setup.** Skiff installs no hooks or plugins and writes nothing into an agent's config or data folder. On request, Resume runs the agent's CLI resume command. An integration, if one exists, is a separate package that the user installs.
- **Restore as shells.** After a daemon restart, each saved session comes back as a shell in its folder. Resume offers the agent's picker, a conversation list, or Continue latest. The user chooses how to resume. Reopening the window reconnects to live sessions.
- **One settings file.** `projects.toml` holds the settings. Customization is declarative: TOML and themes. Skiff has no plugins and no automations.

## Visual rules

- **Square and flush.** `border-radius: 0` everywhere. Dots and slider thumbs are round. Panes touch, with no gaps and no floating cards.
- **A line marks a region edge.** The sidebar, the terminal area, the top bar and the panes have lines between them.
- **Show only what needs a look.** An idle session has no mark unless it has unread output. A shell at its prompt has no mark. The bell means an agent asks for approval. The flag means a turn or a command ended out of sight. Pane buttons show on the focused pane and on a hovered header. A changed file's folder shows on the current row and on hover.
- **One rule weight.** Lines are 1px `--rule`. Pane dividers and edges of floating surfaces use `--rule-strong`. Do not stack accent lines.
- **One row grid.** Main rows, menu items and pane headers are `--row` high (40px at zoom 1). Session rows, file rows and section headers use 20px. The top bar is 48px. Standard side padding is 16px; nested rows use indentation.
- **Settings grid.** A Settings pane uses only 40px multiples: no 20px lines, no gaps; section heads are 40px tinted bands.
- **Everything zooms.** Write lengths as `calc(N * var(--u))` and font sizes as `calc(Npx * var(--zoom, 1))`. Rules and small accent bars use fixed pixel widths. Measured positions use pixels.
- **Tokens only.** Colors come from `styles/tokens.css`. A theme changes the tokens. The color picker uses fixed white and black for its cursor contrast.
- **Typefaces.** IBM Plex Sans for the UI: 13px body, 11 to 12px for meta text and hints, weights 500 to 700 for emphasis. JetBrains Mono for terminals and code. The wordmark uses Space Grotesk.
- **Each color has one job.**
  - Project color (`--pc`) shows location: the project's icon or letters, the current row and the lit menu item.
  - State colors show session state: amber for waiting, blue for working, grey for done and idle. Project colors stay away from state colors.
  - Red shows errors, destructive items and deleted lines in diffs. Green marks added lines and unread output. A destructive item stays red when lit.
- **Highlight, do not outline.** The current row and the lit menu item get a 22% project-color tint and a 2px inset bar on the left. A focus ring shows only after keyboard use (`body.kbd`), as a quiet 1px line. A selected pane gets a 2px frame in the working blue. The focused pane gets one in its project color. While the keys are on another row, the row of the pane on screen keeps the 2px bar without the tint.
- **Shadows on floating surfaces only.** Menus and dialogs have shadows. Menus are solid, never translucent.
- **Little motion.** Small transitions take 120ms. Focus mode and maximize take 180ms and obey reduced-motion settings. The working skiff rocks with `steps()`. While a Skipper idles and its watcher follows open work, its model icon turns once in 8 s, in steps. A Skipper's face blinks and sometimes turns its head. A done face does not move. Bells and flags stay still. Reduced motion stops every loop. Smooth loops cost frames on WebKit and in power-saver mode.
- **Icons.** The Rune Icons outline set, stroke `currentColor`, 14px in menus. Add only the icons that the code uses.

## Theme scope

Themes cascade: app, then project, then session. A lower level replaces the one above it. A group theme sets the theme of each member session.

- Each picker names its scope in the title: "Project theme: skiff", "Session theme: Sea Dog Mermaid".
- The first card is the level above: "Same as app", "Same as project".

Themes are location cues. One look tells you which project or session you are in.

## Interaction rules

- **Keyboard and mouse are equal.** An action has one implementation. Its key, its menu item and its palette entry all call it.
- **Ctrl+Shift for the app.** App keys are Ctrl+Shift+letter (⌘+Shift on macOS). Bare Ctrl belongs to the terminal. Users override keys in `[keys]`, and F1 shows the live keymap. Never hardcode a key label: use `keyLabel(action)`.
- **Keys act on what is highlighted.** The menu and theme keys use the highlighted scope. New session uses the row's worktree, else the focused pane's worktree. End session uses a highlighted session row, else the focused session. Close pane follows the row with DOM focus, else the focused pane or picked group. File-manager and copy-path keys use the highlighted file when there is one. See the [Glossary](https://github.com/solutionscay/skiff/wiki/Glossary).
- **Menus show their keys, and the keys work.** An item's hint key picks that item while the menu is open. Enter always picks the lit item.
- **Only a row with children folds.** A row with nothing under it has no chevron, no fold, and no fold key. A row whose click does nothing does not light on hover.
- **Two views of the side panel.** The tree, or the Skippers list: the Skiff agent and every Skipper, one row each. TREE | SKIPPERS at the top and Ctrl+Alt+Shift+S switch them, and the choice lasts across restarts. A row adds no band or line for the state. The model icon and the time show it.
- **One highlight.** The list has one current row at most. The focused pane's frame hides when the keys are elsewhere, such as on a group row.
- **Focus goes back.** A menu or dialog gives the keys back to the row or terminal that opened it. It does not change the selection.
- **Two regions.** The list and the terminal have one tab stop each. F6 steps between them.
- **Ask before you destroy.** Actions that end live sessions or remove a project ask. They say which sessions end and what stays. Removing a selection of already exited sessions needs no confirmation. Other actions do not ask.

## Words

- Use the Glossary terms. Do not invent synonyms.
- Write in Simplified Technical English: short sentences, active voice, one idea per sentence.
- No help text. A screen shows labels, counts and one error line for each fault. It has no notes and no advice lines.
- Use sentence case. An item that opens a dialog or more choices ends in "…".
- Name the scope: "Session theme", "Project theme", "Close the pane. The session keeps running."
- Keep the tone plain and dry. Leave out hype.
- Words come from the catalog: `t("key")` from `i18n`. Add each new line to `i18n/en.ts` and to the other catalogs (`es.ts`, `zh.ts`); a catalog without the line shows English. Fill names with `{name}` placeholders and counts with `tn()` plural lines, not string joins. Match code on ids, never on the words.

## Before you ship a UI change

- Lines align with the row grid and the 1px rules.
- Colors and sizes come from tokens and zoom units.
- The action has a key, a menu item and a palette entry where it fits, and all three call the same code.
- The key acts on the highlighted target, and the title or label names that target.
- Focus and selection are the same after the menu or dialog closes.
- Nothing new opens in a pane, and nothing writes into an agent's setup.
