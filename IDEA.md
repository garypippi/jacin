# Custom IME for Linux Wayland

## Context / Goal

I want to implement a custom IME (Input Method Editor) on Linux Wayland, purely for fun and learning.

- **Target environment:** Hyprland (wlroots-based compositor)
- **Explicit non-dependencies:** fcitx, ibus (too large; I want to understand and control the IME stack myself)
- **Backend idea:** Neovim running headless, with the Wayland-facing IME acting as a thin frontend

---

## Understanding So Far

### Wayland IME Architecture

On Wayland, IME is compositor-controlled:

- **Applications** (GTK/Qt/Electron) talk to the compositor via `zwp_text_input_v3`
- **IME** talks to the compositor via the privileged protocol `zwp_input_method_v2`
- Only one IME client can bind `zwp_input_method_v2`, and the compositor decides who is allowed

### GNOME / KDE Situation

- GNOME (Mutter) and KDE (KWin) do **not** allow arbitrary IME clients to bind `zwp_input_method_v2`
- In practice, they only support IBus or Fcitx as the IME bridge
- This is enforced at the compositor level for security reasons (not application-level)
- XIM is X11-only and effectively dead
- UIM on Wayland runs through IBus/Fcitx, so it doesn't avoid them

**Conclusion:** A custom standalone IME cannot work on GNOME/KDE Wayland without going through ibus/fcitx.

### Why Hyprland / wlroots Works

- Hyprland uses wlroots and does **not** hardcode an IME provider
- A custom Wayland client can directly bind `zwp_input_method_v2`
- This makes it possible to implement a fully custom IME without fcitx/ibus

---

## Planned Architecture

```
Keyboard
  ↓
Hyprland (wlroots)
  ↓
Custom IME (Wayland client)
  - binds zwp_input_method_v2
  - handles key events
  - manages preedit / commit
  - renders candidate UI (layer-shell or xdg_popup)
  ↓
Hyprland
  ↓
Applications (via zwp_text_input_v3)
```

### IME Frontend Responsibilities

- Wayland protocol handling
- Key state machine
- Preedit lifecycle
- Candidate window UI

### IME Backend: Neovim + vim-skkeleton

- **Neovim** (headless, `--embed` mode)
- **vim-skkeleton** plugin for SKK (Simple Kana to Kanji) conversion
- IPC via msgpack-rpc (nvim-rs crate)

```
Key events → Neovim (skkeleton) → preedit/commit responses
```

SKK is a Japanese input method where:
- Lowercase = hiragana
- Uppercase = start kanji conversion
- Simple, modal, programmer-friendly

---

## Non-Goals / Constraints

- Not aiming for GNOME/KDE compatibility
- Not using fcitx or ibus
- Not using XIM
- This is a learning / experimental project, not production-ready

---

## Open Questions

- wlroots / Wayland protocol details for IME implementation
- Common pitfalls when implementing `zwp_input_method_v2`
- Designing IME UI positioning (cursor rect, candidate window)
