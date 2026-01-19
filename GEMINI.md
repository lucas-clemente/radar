# Description

`Radar` is the backend for a for-fun esphome project: An epaper display mounted in my living room that shows the flight currently passing overhead for my two small kids.

This backend runs a webserver that renders a 1600x1200 image for the Spectra 6 EPD to display. It exposes an /image.svg endpoint that shows the content to be displayed.

# Architecture

- Implemented in rust
- Uses [OpenSky](https://openskynetwork.github.io/opensky-api/rest.html) to show the closest flight, centered around Weiningen ZH, CH (47.4197° N, 8.4344° E).
- Displays the plane's information in an SVG.
- Uses [planespotters.net](https://www.planespotters.net/photo/api) to retrieve an image of the plane.
- Exposes an `/image.png` endpoint that renders the SVG to a PNG using `resvg` and `tiny-skia`.
- To enable PNG rendering of external images, plane photos are fetched by the backend and embedded directly into the SVG as base64 data URIs.
- Performance: System fonts are loaded once into a shared `fontdb` at startup to ensure fast PNG rendering.
- Uses adsbdb.com to retrieve the flight origin and destination data

