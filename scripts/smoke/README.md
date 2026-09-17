# macOS startup checks

`node scripts/smoke/macos.mjs /absolute/path/Torto.app /absolute/output/directory`

Runs the bundled executable against a fresh shelf, an original generated EPUB
(Chinese/English text and PNG) and PDF (text and vector graphics), then launches
the bundle through Launch Services (`open -n -W -a`). A fifth scenario sends an
EPUB through `open -a APP BOOK` to verify Finder file-open event handling without
passing a book on the application's command line. Each scenario uses a new
profile; it must not reuse an earlier output directory. Node and macOS system
tools are the only script dependencies.

`--smoke-test OUTPUT [BOOK]` opts the ordinary application into the check. It
does not replace the window, GPU, parser or renderer. A successful surface
presentation and GPU completion start a five-second observation period. Book
checks also require the reading texture to have been rendered and presented on
a subsequent frame. Application errors, GPU errors, early exit and the 60-second
internal deadline fail the check. The harness enforces a separate 90-second
deadline in case initialization or the event loop hangs. Missing success reports
and nonzero exits fail, even when `open` itself succeeds.

Reports, console output, GPU information, optional screenshots and newly created
Torto crash reports are retained as workflow artifacts. Screenshot availability
does not decide pass/fail. Both architectures must pass before publishing macOS
assets. Main/manual runs check the built app; tag runs check the app copied out
of the final DMG, without modifying or rebuilding the release afterward.

The runner must provide a working window server and GPU. Lack of either is a
failure, not a silent skip or software-rendering substitution. These checks do
not replace Developer ID signing/notarization or testing downloaded apps under
Gatekeeper on independent Macs. They test macOS 15, not older systems.
