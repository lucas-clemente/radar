# Radar

Backend for an ESPHome-driven e-paper display (Spectra 6) showing real-time flight data for aircraft passing over Weiningen ZH, CH (47.4197° N, 8.4344° E).

## Architecture

- **Language:** Rust (Axum web framework).
- **Flight Data:** Fetches the closest aircraft within a ~50km box via [OpenSky Network](https://openskynetwork.github.io/opensky-api/rest.html).
- **Metadata:** Retrieves flight routes (origin/destination) from [adsbdb.com](https://api.adsbdb.com) and aircraft photos from [planespotters.net](https://www.planespotters.net/photo/api).
- **Rendering:** 
    - Generates dynamic SVGs representing flight info and aircraft imagery.
    - Uses `usvg`/`resvg` for SVG-to-raster conversion.
    - `tiny-skia` for pixel-level operations.

## Endpoints

- `/`: Simple HTML index with endpoint links.
- `/image.svg`: Returns the raw SVG representation.
- `/image.png`: Returns a 1600x1200 high-color PNG.
- `/image_dithered.png`: Returns a 1600x1200 PNG optimized for the Spectra 6 EPD using Floyd-Steinberg dithering against a fixed 6-color palette (Black, White, Yellow, Red, Blue, Green).