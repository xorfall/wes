# Bundled assets

The client bundles PT Mono and IBM Plex Sans subsets from Google Fonts. Their
copyright and SIL Open Font License notices are shipped unchanged with the client:

- [PT Mono notice](gui/public/licenses/PT-Mono-OFL.txt), [upstream](https://github.com/google/fonts/tree/main/ofl/ptmono).
- [IBM Plex Sans notice](gui/public/licenses/IBM-Plex-Sans-OFL.txt), [upstream](https://github.com/google/fonts/tree/main/ofl/ibmplexsans).

Other listed font families are resolved from the local operating system and are
not bundled. The desktop icons are project-generated assets. Dependency source
and license metadata are resolved through Cargo and npm lockfiles; this notice
does not replace the licenses of those dependencies.

The terminal recovery model includes modified source from asciinema's avt 0.18.0,
© 2019 Marcin Kulik, under the Apache License 2.0. Its
[license](vendor/avt/LICENSE) and [local change notice](vendor/avt/WES.md) are included
with the source. The original upstream commit is
`567ba32d8cfe4fe4ff61967a0dbbb9c827bcb3a8`.
