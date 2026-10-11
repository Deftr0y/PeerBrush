# Canvas view and pressure controls

Development additions are unreleased.

The **View** menu rotates or mirrors the displayed canvas without changing artwork, document coordinates, undo history or saved PSD sources. Rotation happens before horizontal/vertical screen mirroring. **Reset canvas view** resets rotation, mirrors, zoom and pan, then frames the canvas. Each open project retains its own view. Brush/mask strokes, selections, paths, sampling and transform handles map through the inverse view into document pixels. Agent images continue to use document coordinates.

Default view keys are **Shift+E/R** for 15° rotation left/right, **Shift+V/N** for horizontal/vertical mirroring and **Shift+F** for reset. Finish a painting, path or transform gesture before changing the view.

Choose **Calibrate pressure…** in brush settings. Minimum force is the dead zone; maximum force is the level that reaches full response. The response curve applies after normalizing and clamping force between these endpoints. The endpoints must be at least one percentage point apart. **Use light force** sets the dead zone just below the most recently reported device force; **Use firm force** uses that force as the full-response level. **Reset pressure** restores the linear 0–100% range.

Draw in the calibration pad to see an actual shared-engine stroke. It uses a separate native-depth scratch document and never changes your artwork or its history. Mouse tests use full force; the gauge distinguishes genuinely supplied device force from mouse input. A stroke freezes its settings until release. Pressure settings are independent for Brush, Eraser, Smudge, Clone and Heal. Completed controls save with the other tool preferences in the instance's state directory, outside PSD sources. Invalid or externally changed preference files remain protected; failures are shown and session settings remain effective. An instance without a state directory keeps preferences in memory.

Hold **S** over the canvas before starting a stroke: move horizontally to adjust brush size, vertically to adjust hardness. The cursor shows the actual tip and numeric size/hardness; this gesture does not paint. Liquify adjusts size only. Brackets continue to adjust size in steps.

Open **View → Keyboard and pen shortcuts…** to assign tool, size, frame, color-swap and view actions to letters, digits or brackets, optionally with Shift. Duplicate bindings are rejected without replacing the prior configuration. **Reset shortcuts and buttons** restores all defaults together. These bindings pause during text entry and while configuring shortcuts. Ctrl/Command actions, Alt sampling/zoom, Space navigation and editing keys retain their established behavior. Shortcut files are versioned, validated and written atomically; unknown or externally changed files are preserved.

The two extra pointer buttons default to temporary Eraser and Pan. Configure the tablet driver to send these extra mouse buttons, or choose None/another supported role in the shortcut window. Releasing the button restores the prior tool after any active stroke finishes; losing focus restores it immediately and cancels an unfinished preview. Middle-mouse and Space navigation remain available.

## Verification limits

The input backend supplies touch pressure when available and extra pointer buttons. It does not expose eraser-end identity, so flipping a physical pen cannot yet be advertised as automatic eraser switching. Configure an extra button for temporary Eraser where supported. Real tablet pressure ranges, driver buttons, eraser-end identity and behavior under load require hardware verification. Synthetic egui input and shared-engine native 8/16-bit tests do not replace that verification or receiving-application PSD round trips.
