# Shell — Feature Specification & Capabilities

**Shell** is a GPU-rendered, hardware-accelerated terminal emulator written in Rust, purpose-built for running multiple autonomous coding agent CLIs (such as Claude Code, Antigravity, and OpenAI Codex) concurrently alongside interactive shells.

This document details all implemented features, subsystems, architectural decisions, and keybindings across the Shell project.

---

## Table of Contents

1. [Agent Detection & Activity Tracking Engine](#1-agent-detection--activity-tracking-engine)
2. [Sidebar & Tab Management](#2-sidebar--tab-management)
3. [Floating Overlays & Command Palette](#3-floating-overlays--command-palette)
4. [Find-in-Buffer Search](#4-find-in-buffer-search)
5. [Native macOS Menu Bar Integration](#5-native-macos-menu-bar-integration)
6. [GPU Rendering & Visual Engine](#6-gpu-rendering--visual-engine)
7. [Terminal Emulation & Line Editing Passthrough](#7-terminal-emulation--line-editing-passthrough)
8. [Clipboard, Selection & Mouse Interactions](#8-clipboard-selection--mouse-interactions)
9. [Theming & Color Science](#9-theming--color-science)
10. [Comprehensive Keyboard Shortcuts & Action Reference](#10-comprehensive-keyboard-shortcuts--action-reference)

---

## 1. Agent Detection & Activity Tracking Engine

Shell’s distinguishing capability is deep, kernel-level awareness of what is running inside each tab’s pty and whether that program is working or waiting for human input.

- **Job-Control Foreground Process Group Polling**:
  - Instead of relying on window title escape sequences (which are untrusted and easily spoofed), Shell queries the pty master's foreground process group via `tcgetpgrp`.
  - On macOS, it reads the process name and path via `proc_pidinfo` / `proc_pidpath`. On Linux, it inspects `/proc/<pgid>/comm` and `/proc/<pgid>/exe`.
  - Follows standard POSIX job control: updates accurately when an agent starts, suspends, resumes, or terminates.

- **Version-Named Install & Symlink Path Unwinding**:
  - Modern agent CLIs are often installed behind versioned directories or symlinks (e.g., Claude Code installed at `~/.local/share/claude/versions/2.1.263`, where the kernel reports the process name as `2.1.263`).
  - Shell walks up the executable's real filesystem path past version patterns and generic directory names (`versions`, `bin`, `libexec`, `release`, `node_modules`, `share`, etc.) to recover the canonical tool name (`claude`).

- **Anti-Flicker Process Debouncing**:
  - Shell scripts, compilers, and builds frequently spawn rapid bursts of short-lived child processes (`git`, `date`, `cargo`, `sleep`).
  - A candidate process name must survive consecutive polls (`CONFIRMATIONS = 2`) before the tab label transitions, preventing distracting visual flicker.

- **Sticky Agent Identity**:
  - Autonomous agents frequently shell out to external tools (formatters, tests, git).
  - Once a tab is identified as an agent (`claude`, `antigravity`/`agy`, `codex`), it retains that identity even when executing child commands, relinquishing it only when control returns to the interactive login shell.

- **Content-Based Visual Churn Fingerprinting**:
  - Naive output checks ("has the pty emitted bytes recently?") fail for agent TUIs because idle agents constantly repaint spinners, elapsed time counters, or token stats.
  - Shell computes a hash fingerprint of the visible text grid while stripping out:
    - Braille and ASCII spinner animation frames
    - Box-drawing borders and divider rules
    - Layout whitespace runs
    - Changing numeric digits (timestamps, counters)
  - A tab only transitions from `Working` to `Waiting` when its actual textual content holds still across consecutive polls.

- **Sub-Agent Count Scraping**:
  - Dynamically scrapes agent status lines (such as Claude Code's `1 agent` / `N agents` indicator) and presents a live sub-agent badge directly on the tab.

- **Waiting Attention Pulse & Accent Bar**:
  - When an agent tab in the background transitions to `Waiting`, Shell marks the tab with `needs_attention = true`.
  - The tab displays an animated breathing pulse and a vertical accent bar to immediately alert the developer that the agent requires input. Focusing the tab clears the attention state.

---

## 2. Sidebar & Tab Management

Shell uses a source-list sidebar design (inspired by native macOS apps like Xcode, Mail, and Arc), replacing conventional horizontal tab strips.

- **Full-Size Content Window Chrome**:
  - On macOS, the window uses `.with_fullsize_content_view(true)` and `.with_titlebar_transparent(true)`.
  - The sidebar reaches the very top of the window behind the native traffic light controls.

- **Two-Line Tab Rows**:
  - **Line 1 (Process Label)**: Shows the current process name or user-assigned custom title, preceded by the activity indicator dot and followed by any sub-agent count badge.
  - **Line 2 (Output Summary)**: Displays the last meaningful line of output from that session.
    - Scans the viewport bottom-up.
    - Automatically ignores empty prompts, command markers (`>`, `$`), spinner frames, box-drawing rules, and the active cursor line.
    - Ellipsized with pixel-precision based on sidebar width using `cosmic-text`.

- **Form-Encoded Activity Dots**:
  - Activity is communicated through geometric form rather than color opacity alone:
    - **Idle**: Small solid dot (4px).
    - **Working**: Enlarged solid dot (6px).
    - **Waiting**: Hollow ring stroke (2px stroke width).

- **Reserved Hue System**:
  - Saturated color in the chrome is strictly reserved for agent identity:
    - **Claude Code**: Terracotta Orange (`#D17838`)
    - **Antigravity CLI (`agy`)**: Purple (`#8F75ED`)
    - **Codex CLI**: Mint Teal (`#4CB89E`)
    - **Shell**: Slate Gray (`#6B6B6B`)
    - **Other / Generic Tools**: Neutral Gray (`#8A8A8A`)

- **Interactive Draggable Divider**:
  - Drag the sidebar's right boundary to resize between 168px and 420px.
  - Features an expanded hit-testing grab zone (±4px) and native `ColResize` cursor.
  - Pty terminals re-calculate and inform child processes of their updated column counts immediately upon drag.

- **Toggle Sidebar Visibility**:
  - Show or hide the sidebar at any time via `Cmd+Shift+T` or the View menu, expanding the terminal grid across the entire window.

- **Custom Tab Renaming**:
  - Open the title editor via `Shift+Cmd+I` to assign custom names to sessions, overriding default process labels while retaining agent state.

---

## 3. Floating Overlays & Command Palette

Shell includes an Arc/Dia-style floating command overlay centered on AI coding workflows and terminal navigation.

- **Command Palette (`Cmd+P`)**:
  - **Running Agent Tabs**: Instant fuzzy-switch list of all active sessions, highlighting tabs requiring attention or running background sub-agents.
  - **Agent Quick-Launchers**: Single-keystroke spawning of `claude`, `antigravity` (`agy`), `codex`, or standard login shells.
  - **Agent Slash Commands & Prompts**: Pre-configured agent commands that can be injected into the active pty:
    - `/plan`: Request a step-by-step implementation plan.
    - `/review`: Request an automated git diff and uncommitted change review.
    - `/goal`: Launch autonomous goal loops.
    - `Interrupt Agent (SIGINT)`: Send `Ctrl+C` to cancel agent turns without reaching for special keys.
  - **Terminal Controls**: Instant trigger for font zooming, scrollback clearing, find overlay, and tab renaming.

- **Visual Fuzzy History Search (`Cmd+R`)**:
  - Automatically loads and parses shell history from disk (`~/.zsh_history`, `~/.bash_history`, etc.).
  - Sub-millisecond fuzzy filtering over past commands with query match highlighting.
  - **Enter**: Submits the command immediately into the active terminal pty.
  - **Tab**: Injects the selected command into the pty without submitting, allowing immediate editing.

- **Tab Inspector (`Cmd+I`)**:
  - Modal overlay displaying live diagnostic data for the current session:
    - Terminal grid dimensions (columns × screen lines)
    - Scrollback line count and history capacity
    - Foreground process name and process group ID (PGID)
    - Classified agent kind and activity state (`Idle`, `Working`, `Waiting`)
    - Sub-agent count and custom title overrides

- **Tab Title Rename Modal (`Shift+Cmd+I`)**:
  - Floating dialog to rename the current tab with full inline editing and immediate sidebar update.

---

## 4. Find-in-Buffer Search

Shell features an in-buffer search overlay (`Cmd+F`) optimized for long terminal sessions.

- **Full Scrollback Search**: Searches across both the visible grid and the 10,000-line historical scrollback buffer.
- **Selection Pre-fill**: If text is selected in the terminal when pressing `Cmd+F`, the first line of the selection is automatically pre-populated as the search query.
- **Incremental Real-Time Matching**: Coordinates of matching cells are recalculated as you type.
- **Visual Match Highlights**: Matches are highlighted directly in the GPU quad pass; the active match is distinguished with an accented border.
- **Search Navigation**:
  - `Cmd+G` or `Enter`: Jump to next match (scrolling terminal viewport automatically).
  - `Cmd+Shift+G` or `Shift+Enter`: Jump to previous match.
  - `Escape`: Close find bar and restore previous scroll position.

---

## 5. Native macOS Menu Bar Integration

Shell implements a complete native macOS menu bar via Cocoa/AppKit C-FFI runtime bindings, dispatching unified `AppAction` events:

- **Application Menu ("Shell")**:
  - *About Shell*: Standard system about panel.
  - *Hide Shell* (`Cmd+H`) / *Hide Others* (`Opt+Cmd+H`) / *Show All*.
  - *Quit Shell* (`Cmd+Q`).

- **File Menu ("Shell")**:
  - *New Window* (`Cmd+N`).
  - *New Tab* (`Cmd+T`).
  - *Close Tab* (`Cmd+W` / `Opt+Cmd+W`).
  - *Close Window* (`Shift+Cmd+W`).
  - *Export Text As...* (`Cmd+S`): Exports the complete scrollback and visible text directly to `~/Downloads/shell-output-<timestamp>.txt`.
  - *Export Selected Text As...* (`Shift+Cmd+S`): Exports highlighted selection to `~/Downloads/shell-selection-<timestamp>.txt`.
  - *Show Inspector* (`Cmd+I`).
  - *Edit Title...* (`Shift+Cmd+I`).
  - *Reset Terminal* (`Opt+Cmd+R` / `ESC [!p` soft reset).
  - *Hard Reset* (`Ctrl+Opt+Cmd+R` / `ESC c` hard reset + scrollback clear).

- **Edit Menu**:
  - *Undo* (`Cmd+Z` / `\x1f`) & *Redo* (`Shift+Cmd+Z` / `\x18\x15`).
  - *Cut* (`Cmd+X`), *Copy* (`Cmd+C`), *Paste* (`Cmd+V`).
  - *Paste Escaped Text* (`Ctrl+Cmd+V`): Automatically escapes whitespace and shell control characters for safe execution.
  - *Paste Selection* (`Shift+Cmd+V`).
  - *Select All* (`Cmd+A`).
  - *Clear to Start* (`Cmd+K` / `\x15`).
  - *Clear Scrollback* (`Opt+Cmd+K`).
  - *Clear Screen* (`Ctrl+Cmd+L` / `\x0c`).
  - *Find...* (`Cmd+F`), *Find Next* (`Cmd+G`), *Find Previous* (`Shift+Cmd+G`).
  - *Use Option as Meta Key* (`Opt+Cmd+O` toggle).

- **View Menu**:
  - *Toggle Sidebar* (`Shift+Cmd+T`).
  - *Default Font Size* (`Cmd+0`), *Bigger* (`Cmd++`), *Smaller* (`Cmd+-`).
  - *Scroll to Top* (`Cmd+Home`), *Scroll to Bottom* (`Cmd+End`).
  - *Page Up* (`Cmd+PageUp`), *Page Down* (`Cmd+PageDown`).
  - *Line Up* (`Opt+Cmd+Up`), *Line Down* (`Opt+Cmd+Down`).
  - *Enter / Exit Full Screen* (`Ctrl+Cmd+F`).

- **Window Menu**:
  - *Minimize* (`Cmd+M`), *Zoom*.
  - *Show Previous Tab* (`Cmd+[`), *Show Next Tab* (`Cmd+]`).
  - *Dynamic Tab Catalogue*: Shows all open tabs formatted as `[✓/●] <user> — <process> — <cols>x<rows>` with direct `Opt+Cmd+1`–`9` hotkeys.

- **Help Menu**:
  - *Command Palette...* (`Cmd+P`).
  - *Fuzzy History Search...* (`Cmd+R`).

---

## 6. GPU Rendering & Visual Engine

Shell renders entirely through a custom WebGPU/wgpu pipeline on macOS (Metal) and Linux (Vulkan).

- **Instanced Quad Pipeline (`quad.wgsl`)**:
  - Cell backgrounds, cursors, selection highlights, divider lines, and all chrome components are drawn via instanced quads.
  - Shader implements analytic Signed Distance Field (SDF) evaluation:
    $$\text{dist} = \min(\max(q_x, q_y), 0.0) + \|\max(q, 0.0)\| - r$$
  - Yields smooth, sub-pixel antialiased rounded corners for tabs and modal dialogs without artifacting or seams between abutting terminal cells.

- **High-Performance Glyph Rendering (`glyphon` + `cosmic-text`)**:
  - Sub-pixel positioned text shaping with Swash font cache and persistent text atlases.
  - Supports all Unicode block and box-drawing glyphs, emojis, bold, italic, and underline styling.

- **Enforced WCAG Readability Floor**:
  - Agent CLIs frequently use muted dark grays chosen for specific third-party themes.
  - Shell analyzes the relative luminance of foreground cell text against its underlying background and enforces a minimum contrast ratio (1.3:1 floor), guaranteeing readability under any color scheme.

- **Zero-Latency Terminal Surface**:
  - Window chrome animates (such as the slow attention pulse on a waiting tab), but terminal surface text never animates or delays frame presentation.

---

## 7. Terminal Emulation & Line Editing Passthrough

- **VTE Emulation Engine**:
  - Powered by `alacritty_terminal` for industry-standard compatibility.
  - 24-bit Truecolor (direct RGB) and full 256-color ANSI support.
  - All standard terminal cursor shapes: Block, Beam, Underline, and Hollow Block.
  - 10,000 lines of scrollback history per session.

- **Uncompromising Shell-Side Readline Passthrough**:
  - Shell-side line editing sequences are tested and guaranteed never to be intercepted by GUI shortcuts:
    - Line movement: `Ctrl+A` (start of line), `Ctrl+E` (end of line).
    - Word deletion: `Ctrl+W` (backward word), `Alt+D` (forward word).
    - Line clearing: `Ctrl+U` (kill to start), `Ctrl+K` (kill to end).
    - Line history: `Ctrl+R` (reverse search in shell), Up/Down ANSI history arrows.
    - Completion: `Tab` and `Shift+Tab`.
    - Undo/Redo: `Ctrl+_` and `Ctrl+/`.

- **Security-Hardened Bracketed Paste**:
  - Normalizes carriage returns and line feeds.
  - When running inside bracketed paste mode, strips raw ESC control bytes so malicious clipboard contents cannot break out of paste encapsulation.

- **OSC 52 Clipboard Integration**:
  - Programs running in the pty (such as `tmux`, `neovim`, or remote SSH sessions) can read and write to the system clipboard via OSC 52 sequences.

---

## 8. Clipboard, Selection & Mouse Interactions

- **Multi-Granularity Mouse Selection**:
  - **Single Click + Drag**: Character-by-character selection.
  - **Double-Click**: Word/semantic token selection.
  - **Triple-Click**: Full-line selection.

- **Smart Right-Click**:
  - Right-clicking when text is selected immediately copies the selection to the clipboard.
  - Right-clicking with no active selection pastes the current clipboard contents into the terminal.

- **Clickable HTTP / HTTPS URLs**:
  - Scans terminal rows for valid web addresses while rejecting untrusted URI schemes (`file://` or custom URI schemes) for security.
  - Correctly parses balanced parentheses inside URLs and strips trailing sentence punctuation (`.`, `,`, `!`, `?`).
  - URLs highlight with an underline and trigger a pointer cursor when the app modifier (`Cmd` on macOS) is held, opening in the default browser upon click.

- **Linux Primary Selection & Middle-Click**:
  - On Linux (X11 / Wayland), completing a selection writes to the primary selection buffer, and middle-click pastes from it.
  - On macOS, middle-click pastes from the main system clipboard.

- **Native `pbcopy` / `pbpaste` Fallbacks**:
  - Transparent fallback to macOS command-line clipboard utilities ensures copying and pasting never fails even across sleep/wake or sandbox restrictions.

---

## 9. Theming & Color Science

- **Derived Chrome Color System**:
  - A theme specifies only the terminal palette. All sidebar colors, borders, modal cards, and UI accents are mathematically derived using neutral mixes and relative luminance equations.
  - Swapping a terminal theme restyles the entire application chrome without the theme needing custom UI definitions.

- **Built-in Palettes**:
  - **`prism` (Default Dark)**: Sampled directly from the application icon; near-black with deep blue undertones, cyan-to-violet accent ramp, and silver cursor.
  - **`prism-light`**: High-contrast daylight companion matching macOS light appearance.
  - **`graphite`**: Minimalist slate monochrome palette.

- **Ghostty Theme Compatibility**:
  - Directly parses Ghostty theme files (`key = value` format) from `~/.config/shell/themes/`. Any theme created for Ghostty functions out of the box.

- **System Appearance Following**:
  - Synchronizes automatically with macOS Dark Mode and Light Mode transitions.

---

## 10. Comprehensive Keyboard Shortcuts & Action Reference

> **Note**: Shortcuts use `Cmd` on macOS, and `Ctrl+Shift` on Linux.

### Tab & Window Management
| Shortcut | Action | Description |
|---|---|---|
| `Cmd+T` | New Tab | Spawn a new login shell tab |
| `Cmd+W` | Close Tab | Terminate current tab session |
| `Opt+Cmd+W` | Close Tab (Alternate) | Close active tab |
| `Shift+Cmd+W` | Close Window | Close window and terminate all sessions |
| `Cmd+1` .. `Cmd+9` | Select Tab 1–9 | Jump directly to specified tab |
| `Opt+Cmd+1` .. `Opt+Cmd+9` | Select Tab (Menu) | Select tab via native Window menu |
| `Cmd+]` / `Tab` | Next Tab | Cycle forward through tabs |
| `Cmd+[` / `Shift+Tab` | Previous Tab | Cycle backward through tabs |
| `Shift+Cmd+T` | Toggle Sidebar | Show or hide the sidebar |
| `Shift+Cmd+I` | Edit Title | Rename active tab |
| `Cmd+I` | Show Inspector | View session details and metrics |

### AI Agent & Command Palette
| Shortcut | Action | Description |
|---|---|---|
| `Cmd+P` | Command Palette | Open Arc/Dia-style launcher and action palette |
| `Cmd+R` | Fuzzy History Search | Fuzzy search and run commands from shell history |
| `Escape` | Dismiss Overlay | Close Command Palette, Find Bar, or Inspector |

### Search & Navigation
| Shortcut | Action | Description |
|---|---|---|
| `Cmd+F` | Find in Buffer | Open scrollback search bar |
| `Cmd+G` / `Enter` | Find Next | Jump to next search match |
| `Shift+Cmd+G` / `Shift+Enter` | Find Previous | Jump to previous search match |
| `Cmd+Home` | Scroll to Top | Scroll viewport to top of scrollback |
| `Cmd+End` | Scroll to Bottom | Scroll viewport to bottom (active prompt) |
| `Cmd+PageUp` / `Shift+PageUp` | Page Up | Scroll up one full screen page |
| `Cmd+PageDown` / `Shift+PageDown` | Page Down | Scroll down one full screen page |
| `Opt+Cmd+Up` | Line Up | Scroll viewport up one line |
| `Opt+Cmd+Down` | Line Down | Scroll viewport down one line |

### Text Editing & Clipboard
| Shortcut | Action | Description |
|---|---|---|
| `Cmd+C` | Copy | Copy selection to clipboard (or send `Ctrl+C` if empty) |
| `Cmd+X` | Cut | Cut selection to clipboard (or kill current line) |
| `Cmd+V` | Paste | Paste clipboard into active terminal |
| `Ctrl+Cmd+V` | Paste Escaped Text | Paste clipboard with escaped shell characters |
| `Shift+Cmd+V` | Paste Selection | Paste primary mouse selection |
| `Cmd+A` | Select All | Select all text across the active grid |
| `Cmd+Z` | Undo | Send readline undo (`\x1f`) |
| `Shift+Cmd+Z` | Redo | Send readline redo (`\x18\x15`) |
| `Cmd+K` | Clear to Start | Clear line to start and reset display |
| `Opt+Cmd+K` | Clear Scrollback | Erase historical scrollback lines |
| `Ctrl+Cmd+L` | Clear Screen | Send form feed (`\x0c`) to clear screen |

### Export & Diagnostics
| Shortcut | Action | Description |
|---|---|---|
| `Cmd+S` | Export Text | Save full terminal buffer to `~/Downloads` |
| `Shift+Cmd+S` | Export Selection | Save highlighted text to `~/Downloads` |
| `Opt+Cmd+R` | Soft Reset | Send DECSTR soft terminal reset (`ESC [!p`) |
| `Ctrl+Opt+Cmd+R` | Hard Reset | Send RIS hard terminal reset (`ESC c`) |
| `Opt+Cmd+O` | Option as Meta | Toggle Option key sending ESC prefix |

### View & Window Controls
| Shortcut | Action | Description |
|---|---|---|
| `Cmd++` / `Cmd+=` | Zoom In | Increase terminal font size |
| `Cmd+-` | Zoom Out | Decrease terminal font size |
| `Cmd+0` | Reset Font | Reset font size to default (13.0pt) |
| `Ctrl+Cmd+F` | Full Screen | Toggle borderless fullscreen |
| `Cmd+M` | Minimize | Minimize application window |
| `Cmd+H` | Hide Application | Hide Shell window |
| `Opt+Cmd+H` | Hide Others | Hide all other applications |
| `Cmd+Q` | Quit | Terminate Shell |
