//! The global capture shortcut.

use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

#[cfg(target_os = "macos")]
use crate::command_keys;
use crate::{Core, capture, intake};

pub const IN_USE: &str = "That shortcut is in use. Try another.";

#[derive(Clone, Copy)]
pub enum Hotkey {
    Keys(Shortcut),
    /// Both Command keys held together.
    #[cfg(target_os = "macos")]
    BothCommands,
}

/// Parses an accelerator such as `CommandOrControl+Shift+Digit2`, or `command_keys::ACCELERATOR`
/// on macOS. Command or Control is required (macOS rejects Option/Shift-only hotkeys, and plain
/// keys would swallow typing).
pub fn parse(accelerator: &str) -> Result<Hotkey, String> {
    #[cfg(target_os = "macos")]
    if accelerator == command_keys::ACCELERATOR {
        return Ok(Hotkey::BothCommands);
    }
    let shortcut: Shortcut = accelerator.parse().map_err(|_| "That key can’t be used in a shortcut.".to_owned())?;
    if !shortcut.mods.intersects(Modifiers::SUPER | Modifiers::CONTROL) {
        return Err("Include Command or Control in the shortcut.".into());
    }
    Ok(Hotkey::Keys(shortcut))
}

fn register(app: &AppHandle, hotkey: Hotkey) -> Result<(), String> {
    match hotkey {
        Hotkey::Keys(shortcut) => app
            .global_shortcut()
            .on_shortcut(shortcut, |app, _, event| {
                if event.state == ShortcutState::Pressed {
                    intake::capture(app);
                }
            })
            .map_err(|_| IN_USE.to_owned()),
        #[cfg(target_os = "macos")]
        Hotkey::BothCommands => command_keys::watch(app),
    }
}

fn unregister(app: &AppHandle, hotkey: Hotkey) {
    match hotkey {
        Hotkey::Keys(shortcut) => {
            let _ = app.global_shortcut().unregister(shortcut);
        }
        #[cfg(target_os = "macos")]
        Hotkey::BothCommands => command_keys::unwatch(),
    }
}

/// Swaps the registered shortcut for `next`. On failure the previous one is restored.
pub fn replace(app: &AppHandle, next: Option<Hotkey>) -> Result<(), String> {
    let core = app.state::<Core>();
    let mut current = core.shortcut.lock().unwrap();
    let previous = current.take();
    if let Some(previous) = previous {
        unregister(app, previous);
    }
    let Some(next) = next else { return Ok(()) };
    if let Err(e) = register(app, next) {
        if let Some(previous) = previous
            && register(app, previous).is_ok()
        {
            *current = Some(previous);
        }
        return Err(e);
    }
    *current = Some(next);
    Ok(())
}

/// While the shortcut recorder is active, pressing the current combo must not capture.
pub fn pause(app: &AppHandle, paused: bool) -> Result<(), String> {
    if !capture::shortcut_supported() {
        return Ok(());
    }
    if paused {
        return replace(app, None);
    }
    let core = app.state::<Core>();
    if core.shortcut.lock().unwrap().is_some() {
        return Ok(());
    }
    let accelerator = core.settings.lock().unwrap().shortcut.clone();
    replace(app, accelerator.as_deref().map(parse).transpose()?)
}

/// Registers the saved shortcut at launch.
pub fn init(app: &AppHandle) {
    if !capture::shortcut_supported() {
        return;
    }
    let accelerator = app.state::<Core>().settings.lock().unwrap().shortcut.clone();
    if let Some(accelerator) = accelerator
        && let Err(e) = parse(&accelerator).and_then(|s| replace(app, Some(s)))
    {
        eprintln!("push: shortcut {accelerator} not registered: {e}");
    }
}
