# Canvas view and pressure controls

Development additions are unreleased.

Open **Canvas view…** in the main toolbar to adjust zoom, frame the canvas or selection, and rotate or mirror the view. Choose **Fit canvas** to center the entire image at its current angle, or **100%** to zoom to one document pixel per display pixel around the viewport center. Drag the zoom control or double-click it to enter an exact percentage. Rotation happens before horizontal or vertical screen mirroring. **Reset canvas view** restores the original orientation and fits the canvas. View changes wait until the active canvas gesture finishes so the press and release use the same view. Each open project retains its own view. Brush strokes, selections, paths, sampling and transform handles map through the inverse view into document pixels. Agent images use document coordinates.

In PeerBrush 0.4, you can start a rectangular or elliptical marquee, freehand lasso, Object region or Quick Selection drag in the gray workspace and move onto the canvas. Magic Wand can start in the gray workspace and samples the canvas pixel where you release. Polygonal Lasso and Magnetic Lasso also place anchors in the workspace. Magnetic Lasso uses the clicked position in the gray workspace and snaps to image edges on the canvas. Click beside the artwork with Single row or above or below it with Single column to select the line at that document coordinate. Selection coverage is limited to the canvas. Brush, eraser, wet blend, clone, heal, liquify, shape, gradient and text-field drags also accept a workspace start. Fill accepts a workspace click and applies to the selected layers and active pixel selection on release. Escape, focus loss, or a changed target cancels that click. Click a canvas pixel to sample a color or set a clone/heal source. A workspace selection click that misses the image clears the selection.

Default view keys are **Shift+E/R** for 15° rotation left/right, **Shift+V/N** for horizontal/vertical mirroring and **Shift+F** for reset. Rotation and mirroring keep the current zoom and the same part of the image centered. An image centered at fit zoom refits to its new angle. Finish a painting, path or transform gesture before changing the view.

For area selection drags, hold Shift to add, Alt to subtract, or both to intersect when you press the pointer. That mode stays active through release. A drag started in the canvas workspace keeps tracking the pointer as it crosses other panels. Transform handles in the workspace respond only where the canvas panel is exposed. Cancelling the contact discards the unfinished transform.

Single row and Single column also accept a press beyond the valid rows or columns. Drag into range and release to select that line. Magic Wand, Single row and Single column use the release position and modifiers whether you start on the image or in the gray workspace.

Brush, eraser, wet blend, clone, heal and liquify bind the stroke to its starting layer and channel. Changing the target or editing the document during the drag cancels that stroke.

Rectangle, ellipse and gradient drags keep their full geometry as they cross the canvas edge. A drag that misses both the canvas and the target layer’s bounds draws only the guide and leaves no undo entry.

In **Image → Crop…**, drag from the gray workspace to draw a new crop box, or move a handle to adjust the existing box. Releasing sets the draft bounds. **Apply** commits the crop.

The Gradient toolbar shows the foreground color fading to transparency in the selected direction. Click the larger ramp to open the color picker, or choose a smaller preview to set the fade direction in document coordinates. The color and direction are captured when you start the drag.

Quick Selection samples the portions of the drag that cross the image. You can leave the image and re-enter without sampling the gray workspace. The live overlay shows the selection as it updates. Release applies it as one edit.

**Set source** and the Eyedropper sample where you release. You can click an image pixel, or press in the gray workspace and release over the image. Clone and Heal’s Alt-click samples the press position. Moving the pointer immediately afterward leaves the chosen source at the sampled pixel.

Choose **Calibrate pressure…** in brush settings. Minimum force is the dead zone; maximum force is the level that reaches full response. The endpoints must be at least one percentage point apart. In Brush and Pressure Calibration, drag the point on the response graph up to get more response from the same force, or down to get less. Double-click the adjacent value to enter the exponent precisely. **Use light force** sets the dead zone just below the most recently reported device force; **Use firm force** uses that force as the full-response level. **Reset pressure** restores the linear 0–100% range.

The brush **Angle** dial shows the tip direction. Drag the dial to turn the tip, hold Shift to snap in 15° steps, or double-click the degree value to type an angle. The brush sample redraws as you adjust these controls.

Draw in the calibration pad to see an actual shared-engine stroke. It uses a separate native-depth scratch document and never changes your artwork or its history. Mouse tests use full force; the gauge distinguishes genuinely supplied device force from mouse input. A stroke freezes its settings until release. Pressure settings are independent for Brush, Eraser, Smudge, Clone and Heal. Completed controls save with the other tool preferences in the instance's state directory, outside PSD sources. Invalid or externally changed preference files remain protected; failures are shown and session settings remain effective. An instance without a state directory keeps preferences in memory.

A quick tap retains its reported force when press and release arrive together. The canvas and calibration pad use force samples at the pointer position. A new mouse stroke starts at full force.

If the device cancels the contact driving the pointer, PeerBrush discards the unfinished brush stroke or calibration preview. Cancellation of another contact leaves the active stroke in place.

When the last movement and release of an active canvas stroke arrive together, the stroke keeps the final movement and its force. Pointer movement after release does not extend it.

Hold **S** over the canvas before starting a stroke: move horizontally to adjust brush size, vertically to adjust hardness. The cursor shows the actual tip and numeric size/hardness; this gesture does not paint. Liquify adjusts size only. Brackets continue to adjust size in steps.

Open **File → Keyboard and pen shortcuts…** to assign tool, size, frame, color-swap and view actions, including **Duplicate with Transform**. Click the current shortcut and press a letter, digit or bracket, optionally with Shift. Escape cancels recording. Use **Keys** to choose a key or a Shift combination with the pointer. Duplicate bindings leave the prior configuration in place. **Reset shortcuts and buttons** restores all defaults together. Custom shortcuts pause during text entry and while configuring shortcuts. Shortcut files are versioned and written atomically, with unknown or externally changed files preserved.

Shortcuts use the key reported by the active keyboard layout, with a physical-key fallback for characters outside the shortcut key set. A held shortcut keeps its original key identity until release, including when the layout changes mid-press. Typed characters follow the active layout.

The two extra pointer buttons default to temporary Eraser and Pan. Configure the tablet driver to send these extra mouse buttons, or choose None/another supported role in the shortcut window. Releasing the button restores the prior tool after any active stroke finishes; losing focus restores it immediately and cancels an unfinished preview. Middle-mouse and Space navigation remain available.

## Verification limits

The input backend supplies touch pressure when available and extra pointer buttons. It does not expose eraser-end identity, so flipping a physical pen cannot yet be advertised as automatic eraser switching. Configure an extra button for temporary Eraser where supported. Real tablet pressure ranges, driver buttons, eraser-end identity and behavior under load require hardware verification. Synthetic egui input and shared-engine native 8/16-bit tests do not replace that verification or receiving-application PSD round trips.
