"""Optional external PeerBrush segmentation provider (GPL-3.0-only).

Model selection and weights belong to the provider configuration, not the editor.
Requires integrations/segmentation-requirements.txt. Reads one JSON request from
stdin and writes one JSON result with PNG confidence data/document coordinates.
"""
import argparse
import base64
import contextlib
import io
import json
import sys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", required=True)
    args = parser.parse_args()
    request = json.loads(sys.stdin.buffer.read(16 * 1024 * 1024 + 1))
    if request.get("version") != 1:
        raise ValueError("Unsupported segmentation request version")
    from PIL import Image
    with contextlib.redirect_stdout(sys.stderr):
        from rembg import new_session, remove
        session = new_session(args.model, providers=["CPUExecutionProvider"])
        image = Image.open(io.BytesIO(base64.b64decode(request["png"], validate=True)))
        if image.width * image.height > 2048 * 2048:
            raise ValueError("Input preview exceeds four megapixels")
        # Inference uses a preview; the editor's original channels are never returned
        # as an editable RGB replacement.
        mask = remove(image, session=session, only_mask=True).convert("L")
        point = request.get("point")
        if point is not None:
            import numpy as np
            from scipy import ndimage
            rect = request["document_rect"]
            x = int((point[0] - rect[0]) * mask.width / (rect[2] - rect[0]))
            y = int((point[1] - rect[1]) * mask.height / (rect[3] - rect[1]))
            if not (0 <= x < mask.width and 0 <= y < mask.height):
                raise ValueError("Object point is outside the requested region")
            values = np.asarray(mask)
            foreground = values >= 128
            labels, _ = ndimage.label(foreground)
            component = labels[y, x]
            if component == 0:
                raise ValueError("Object point is outside the learned foreground")
            nearest = ndimage.distance_transform_edt(
                ~foreground, return_distances=False, return_indices=True
            )
            selected = labels[tuple(nearest)] == component
            mask = Image.fromarray(np.where(selected, values, 0).astype("uint8"))
    encoded = io.BytesIO()
    mask.save(encoded, format="PNG")
    json.dump({"version": 1, "document_id": request["document_id"],
               "source_revision": request["source_revision"],
               "document_rect": request["document_rect"], "channel": "luma",
               "png": base64.b64encode(encoded.getvalue()).decode("ascii")}, sys.stdout)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        # Model/download errors stay in the provider; stdout remains valid protocol.
        print(f"Segmentation provider: {type(error).__name__}: {error}", file=sys.stderr)
        sys.exit(1)
