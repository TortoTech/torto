<p align="center">
  <strong>English</strong> · <a href="README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <img src="assets/windows/torto-128.png" width="112" height="112" alt="Torto app icon">
</p>

<h1 id="torto" align="center">Torto</h1>

<p align="center">
  A local-first ebook reader built around Focus Mode—one meaningful passage at a time.<br>
  Native Rust rendering, no WebView, and your library stays under your control.
</p>

<p align="center">
  <a href="https://github.com/TortoTech/torto/releases/latest"><img src="https://img.shields.io/github/v/release/TortoTech/torto?display_name=tag&sort=semver" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/TortoTech/torto" alt="MIT License"></a>
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS-5b6ee1" alt="Windows and macOS">
  <img src="https://img.shields.io/badge/UI-egui-7c3aed" alt="Built with egui">
</p>

<p align="center">
  <a href="#focus-mode">Focus Mode</a> •
  <a href="#features">Features</a> •
  <a href="#screenshots">Screenshots</a> •
  <a href="#download">Download</a> •
  <a href="#privacy">Privacy</a> •
  <a href="#development">Development</a> •
  <a href="#project-status">Project status</a> •
  <a href="#license">License</a>
</p>

## About

Torto is an open-source desktop reader designed around **Focus Mode**. Instead of treating a book as a stack of pages, Focus Mode turns its structure into meaningful reading units—paragraphs, nested lists, quotations, code blocks, tables, and images—and keeps the current unit in a stable reading position. Navigation, highlights, notes, footnotes, translation, and AI conversation all follow that context.

Classic single-column, two-column, and vertical-scroll layouts remain available when you want a conventional page or chapter view. The local bookshelf, full-book search, translation, optional AI assistant, PDF OCR, and direct WebDAV sync are built around the same native reading model.

Unlike browser-based readers, Torto parses, lays out, paginates, and renders book content through a native Rust pipeline built on egui, Parley, Vello, and wgpu.

## Focus Mode

Focus Mode is Torto's default and flagship reading experience—and the feature that most clearly sets it apart from conventional ebook readers.

- **Read by meaning, not by page.** One semantic unit is active at a time. Images are valid reading units, nested list descendants stay with their parent item, and large tables or media remain scrollable instead of being skipped.
- **Keep your place visually.** Short units begin at a stable position inside a centered reading stage; taller content expands only when it needs more room. Moving between units avoids the constant vertical jump caused by paragraphs of different lengths.
- **Navigate consistently.** The mouse wheel, arrow keys, and the reader scrollbar move through reading units. When a table of contents, chat, footnote panel, or text editor owns input, scrolling stays inside that surface.
- **Act on the current passage.** Open the action toolbar with `Space`, chat with `Tab`, highlight with `1`, add a note with `2`, use **Split by sentence** with `3`, and toggle contextual footnotes with `Left Alt`. Shortcuts are configurable.
- **Keep context attached.** Focus chat sessions are scoped to their reading unit for the current book session, while highlights and notes persist at stable source locations. Footnotes are reduced to compact icons on the active unit and expanded only on demand.
- **Bring scanned PDFs into the same workflow.** Original-layout PDFs use Classic Mode. After PDF OCR creates a reflowable text layer, Focus Mode becomes available with the same unit navigation and reading tools.

## Features

<div align="left">✅ Implemented</div>

| **Feature** | **Description** | **Status** |
| --- | --- | --- |
| **Focus Mode** | Read one semantic unit at a stable position, navigate by unit, and keep actions, conversations, notes, and footnotes attached to the current context. | ✅ Core |
| **Multi-format support** | Read DRM-free EPUB, MOBI, AZW, AZW3/KF8, FB2, FBZ, CBZ, CHM, and PDF files. | ✅ |
| **Native rendering** | Parse, lay out, paginate, and render books with Rust instead of embedding a browser or WebView. | ✅ |
| **Classic layouts** | Switch between single-column, two-column, and section-based vertical scrolling layouts, with unified typography or the book's authored styles. | ✅ |
| **Cover-first library** | Import books in bulk, browse cover cards, search by title or author, resume recent reading first, and detect duplicates. | ✅ |
| **Navigation and search** | Use a hierarchical table of contents, chapter tracking, full-book search, keyboard navigation, mouse-wheel paging, and `F11` fullscreen. | ✅ |
| **Typography and themes** | Configure default, CJK, code, and interface fonts; choose unified or book-authored typography; and follow the system, Light, or Dark theme. | ✅ |
| **Selection and annotations** | Select freely or by word, sentence, or paragraph; copy text, create highlights and notes, and return to durable source locations. | ✅ |
| **Image preview** | Open book images in an overlay, zoom with the wheel, pan when enlarged, and copy images to the clipboard. | ✅ |
| **Translation** | Translate book content in replacement or bilingual mode and translate the table of contents. | ✅ |
| **AI reading assistant** | Stream answers with source-backed citations and render Markdown, tables, math, SVG, and Mermaid, including enlarged visual previews. | ✅ Optional |
| **PDF OCR and metadata recognition** | Recognize title, author, table of contents, page roles, and body content; switch between the original PDF and a reflowable OCR layout. | ✅ Optional |
| **WebDAV sync** | Sync books, reading progress, highlights, and notes directly through your own WebDAV provider. | ✅ Optional |
| **Windows updates** | Check GitHub Releases automatically, verify the MSI with SHA-256, and install updates after confirmation. | ✅ Windows |

## Screenshots

### Local bookshelf

Import books, browse covers, search by title or author, and continue where you left off.

![Torto local ebook library](assets/screenshots/library.png)

### Classic two-column layout

Focus Mode is the default experience described above. Torto also keeps a conventional two-column layout for readers who prefer a page-like view.

![Torto classic two-column reader](assets/screenshots/reader.png)

## Download

Download the latest build from [GitHub Releases](https://github.com/TortoTech/torto/releases/latest).

| **Platform** | **Package** | **Requirements** |
| --- | --- | --- |
| Windows | `Torto-*-x86_64.msi` | 64-bit Windows 10 or 11 |
| macOS, Apple silicon | `Torto-*-macos-arm64.dmg` | macOS 12 or later |
| macOS, Intel | `Torto-*-macos-x86_64.dmg` | macOS 12 or later |

After installation, import one or more books from the bookshelf and open a book in Focus Mode. Use `↑` / `↓`, the mouse wheel, or the reader scrollbar to move between reading units; press `Space` for passage actions and `Tab` for passage-scoped chat. Use `Ctrl + F` to search the current book and the reader menu to change layout, typography, theme, translation, AI, OCR, shortcuts, and sync settings.

## Privacy

- Imported books and reading data remain in Torto's local application-data directory.
- WebDAV passwords and AI API keys are stored in the operating system's secure credential store rather than ordinary configuration files.
- AI and translation features are opt-in. Content is sent only when you configure and actively use a provider.
- WebDAV traffic goes directly from Torto to the service you choose; there is no Torto-operated relay.

## Development

Torto uses Rust `1.97.1`. The workspace package remains named `rebook-desktop`, while the shipped application is `Torto` (`torto.exe` on Windows).

```powershell
cargo run --locked -p rebook-desktop
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

The core reading path is `parser → Reading IR → layout → renderer`. See the [native rendering architecture decision](docs/adr-0001-native-epub-renderer.md), [WebDAV sync protocol](docs/webdav-sync-v1.md), and [known upstream issues](docs/known-upstream-issues.md) for details.

## Project status

Torto is under active development. DRM-protected books are not supported. Focus Mode is unavailable for an original-layout PDF until reflowable OCR content exists. The native renderer intentionally does not aim for full browser-level HTML/CSS compatibility, so complex fixed layouts, vertical writing, Ruby annotations, and some interactive book content may not render completely yet.

Please report reproducible problems in [Issues](https://github.com/TortoTech/torto/issues). Include the book format, screenshots, and reproduction steps, but do not upload complete copyrighted books.

## License

Torto is available under the [MIT License](LICENSE).
