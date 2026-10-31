# Original Aseprite fixture

`layers.aseprite` is an original 304-byte synthetic file, authored for PhotoCraft #3811
from the public Aseprite file-format specification. No Aseprite application code or assets
were used. It is licensed under MIT OR Apache-2.0, like PhotoCraft; see the adjacent license.

The canvas is 2 × 2 RGBA8, one frame. Header flags are 3 (valid layer opacity and isolated
groups). Bottom-to-top layers: `Base`, `Group`, and `Top` inside `Group`. Base contains four
opaque red pixels. Top is at (1,0), size 1 × 2, containing blue alpha 128 and opaque green.
The two cels are raw. The expected composite, row-major RGBA8, is approximately:
`(255,0,0,255), (127,0,128,255), (255,0,0,255), (0,255,0,255)`.

The independent MIT OR Apache-2.0 `aseprite-io` 0.2.0 reader also accepts the file and agrees
on canvas dimensions, frame count, layer names and parent indices. The unit tests build
additional fixtures in memory, including compressed and grayscale data.

`gray.aseprite` is an original 3 × 2 grayscale sprite generated using the independent
MIT OR Apache-2.0 `aseprite-io` 0.2.0 writer (no third-party artwork). Its one compressed
3 × 1 cel at (-1,1) has samples `(32,0), (128,128), (240,255)` in Gray/Alpha order.
The layer is named `Gray from independent writer`. Its exact samples are tested through
import and a native-format round trip. That library is a test-generation tool only, not
an app or test-suite dependency.
