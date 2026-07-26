# Browser USB-PD sink control client scripts

Run these PowerShell scripts from the repository root:

| Script | Purpose |
|---|---|
| `package.ps1` | Bundle the HTML, CSS, protocol decoder, and application into one offline HTML file |
| `launch.ps1` | Package that file, open it in the default browser, and exit |

The default output is
`examples/generated-artifacts/usb-pd-control.html`. Pass `-Output <path>` to
either script to choose another destination, or pass `-NoBrowser` to
`launch.ps1` to package without opening a browser.

No localhost server is started. Desktop Web Serial works from the standalone
file; Android WebUSB requires the HTTPS-hosted GitHub Pages copy described in
the parent [browser-client README](../README.md).
