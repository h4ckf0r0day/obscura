# Media preference emulation

`Emulation.setEmulatedMedia` accepts `prefers-color-scheme` and
`prefers-reduced-motion` in its `features` array:

```json
{"features":[{"name":"prefers-color-scheme","value":"dark"},{"name":"prefers-reduced-motion","value":"reduce"}]}
```

Color-scheme values are `dark`, `light`, `no-preference`, and the empty string.
The last two restore the default light preference; the obsolete CSS query
`(prefers-color-scheme: no-preference)` does not match. Reduced-motion values
are `reduce`, `no-preference`, and the empty string.

A supplied array replaces both supported preferences. `features: []` clears
both overrides. Omitting `features` also clears them. Invalid supported values
or malformed feature entries return an error before changing page state.

Overrides persist across navigation and apply to existing and newly created
child documents. `matchMedia()` results and change events work with and without
rendering. Render builds also update CSS media selection, live computed styles,
geometry, screenshots (including ad-hoc capture viewports), and the preference
used during PDF print layout.
The Rust `Page::set_media_preferences(reduce, dark)` method applies the same
pair of preferences directly.

This does not implement touch emulation or the CDP `media` parameter. Other
media features retain their existing behavior. The CSS `color-scheme` property
and automatic darkening are separate from this media-query override. Dynamically
loaded CSS imports still use the loader's existing fetch-time media filter;
a preference change does not refetch an import previously skipped by it.
