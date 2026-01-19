use axum::{
    Router,
    body::Body,
    extract::State,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tiny_skia::Pixmap;
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use usvg::{Tree, fontdb};

#[derive(Clone)]
struct AppState {
    usvg_options: Arc<usvg::Options<'static>>,
}

const FONT_DATA: &[u8] = include_bytes!("../GoogleSans-VariableFont_GRAD,opsz,wght.ttf");

const PALETTE: [[u8; 3]; 6] = [
    [0, 0, 0],       // Black
    [255, 255, 255], // White
    [255, 255, 0],   // Yellow
    [255, 0, 0],     // Red
    [0, 0, 255],     // Blue
    [0, 255, 0],     // Green
];

const LAT: f64 = 47.41876326848794;
const LON: f64 = 8.426291132310645;
const BOX_SIZE: f64 = 0.1; // Roughly 10km
const MAX_ALTITUDE_METERS: f64 = 6096.0; // 20,000 feet

#[derive(Debug, Deserialize)]
struct OpenSkyResponse {
    states: Option<Vec<Vec<serde_json::Value>>>,
}

#[derive(Debug, Deserialize)]
struct AdsbdbResponse {
    response: AdsbdbData,
}

#[derive(Debug, Deserialize)]
struct AdsbdbData {
    flightroute: Option<AdsbdbFlightRoute>,
    aircraft: Option<AdsbdbAircraft>,
}

#[derive(Debug, Deserialize)]
struct AdsbdbAircraft {
    #[serde(rename = "type")]
    aircraft_type: String,
}

#[derive(Debug, Deserialize)]
struct AdsbdbFlightRoute {
    origin: AdsbdbAirport,
    destination: AdsbdbAirport,
    callsign_iata: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AdsbdbAirport {
    iata_code: String,
    municipality: String,
}

#[derive(Debug, Deserialize)]
struct PlanespottersResponse {
    photos: Vec<PlanespottersPhoto>,
}

#[derive(Debug, Deserialize)]
struct PlanespottersPhoto {
    thumbnail_large: PlanespottersImage,
}

#[derive(Debug, Deserialize)]
struct PlanespottersImage {
    src: String,
}

#[derive(Debug, Clone)]
struct Flight {
    icao24: String,
    callsign: String,
    flight_number: Option<String>,
    aircraft_type: Option<String>,
    distance: f64,
    photo_url: Option<String>,
    photo_base64: Option<String>,
    origin_iata: Option<String>,
    origin_name: Option<String>,
    dest_iata: Option<String>,
    dest_name: Option<String>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "radar=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let mut fontdb = fontdb::Database::new();
    fontdb.load_font_data(FONT_DATA.to_vec());
    let mut usvg_options = usvg::Options::default();
    usvg_options.fontdb = Arc::new(fontdb);

    let state = AppState {
        usvg_options: Arc::new(usvg_options),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/image.svg", get(get_image))
        .route("/image.png", get(get_image_png))
        .route("/image_dithered.png", get(get_image_dithered_png))
        .route("/image.bin", get(get_image_bin))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let addr: SocketAddr = format!("0.0.0.0:{}", port).parse().unwrap();
    info!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn index() -> Html<&'static str> {
    Html(
        "<h1>Radar</h1><ul><li><a href='/image.svg'>/image.svg</a></li><li><a href='/image.png'>/image.png</a></li><li><a href='/image_dithered.png'>/image_dithered.png</a></li><li><a href='/image.bin'>/image.bin</a></li></ul>",
    )
}

async fn get_image(_state: State<AppState>) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let fetch_result = fetch_closest_flight().await;
    let fetch_duration = start.elapsed();

    match fetch_result {
        Ok(Some(flight)) => {
            let render_start = std::time::Instant::now();
            let svg = render_svg(&flight);
            let render_duration = render_start.elapsed();

            info!(
                "Request processed: fetch={:?}, render_svg={:?}",
                fetch_duration, render_duration
            );

            Response::builder()
                .header("Content-Type", "image/svg+xml")
                .header("Cache-Control", "no-cache, no-store, must-revalidate")
                .body(svg)
                .unwrap()
        }
        Ok(None) => {
            let svg = render_no_flight_svg();
            info!("No flight found: fetch={:?}", fetch_duration);
            Response::builder()
                .header("Content-Type", "image/svg+xml")
                .header("Cache-Control", "no-cache, no-store, must-revalidate")
                .body(svg)
                .unwrap()
        }
        Err(e) => {
            error!("Error fetching flight: {} (took {:?})", e, fetch_duration);
            Response::builder()
                .status(500)
                .body(format!("Error: {}", e))
                .unwrap()
        }
    }
}

async fn get_image_png(State(state): State<AppState>) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let fetch_result = fetch_closest_flight().await;
    let fetch_duration = start.elapsed();

    match fetch_result {
        Ok(Some(flight)) => {
            let svg_start = std::time::Instant::now();
            let svg = render_svg(&flight);
            let svg_duration = svg_start.elapsed();

            let png_start = std::time::Instant::now();
            match svg_to_png(&svg, &state.usvg_options) {
                Ok(png) => {
                    let png_duration = png_start.elapsed();
                    info!(
                        "Request processed (PNG): fetch={:?}, render_svg={:?}, render_png={:?}",
                        fetch_duration, svg_duration, png_duration
                    );

                    Response::builder()
                        .header("Content-Type", "image/png")
                        .header("Cache-Control", "no-cache, no-store, must-revalidate")
                        .body(Body::from(png))
                        .unwrap()
                }
                Err(e) => {
                    error!("Error rendering PNG: {}", e);
                    Response::builder()
                        .status(500)
                        .body(Body::from(format!("Error rendering PNG: {}", e)))
                        .unwrap()
                }
            }
        }
        Ok(None) => {
            let svg = render_no_flight_svg();
            match svg_to_png(&svg, &state.usvg_options) {
                Ok(png) => {
                    info!("No flight found (PNG): fetch={:?}", fetch_duration);
                    Response::builder()
                        .header("Content-Type", "image/png")
                        .header("Cache-Control", "no-cache, no-store, must-revalidate")
                        .body(Body::from(png))
                        .unwrap()
                }
                Err(e) => {
                    error!("Error rendering PNG: {}", e);
                    Response::builder()
                        .status(500)
                        .body(Body::from(format!("Error rendering PNG: {}", e)))
                        .unwrap()
                }
            }
        }
        Err(e) => {
            error!("Error fetching flight: {} (took {:?})", e, fetch_duration);
            Response::builder()
                .status(500)
                .body(Body::from(format!("Error: {}", e)))
                .unwrap()
        }
    }
}

async fn get_image_dithered_png(State(state): State<AppState>) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let fetch_result = fetch_closest_flight().await;
    let fetch_duration = start.elapsed();

    match fetch_result {
        Ok(Some(flight)) => {
            let svg = render_svg(&flight);
            match svg_to_dithered_png(&svg, &state.usvg_options) {
                Ok(png) => {
                    info!(
                        "Request processed (Dithered PNG): fetch={:?}, total={:?}",
                        fetch_duration,
                        start.elapsed()
                    );
                    Response::builder()
                        .header("Content-Type", "image/png")
                        .header("Cache-Control", "no-cache, no-store, must-revalidate")
                        .body(Body::from(png))
                        .unwrap()
                }
                Err(e) => {
                    error!("Error rendering dithered PNG: {}", e);
                    Response::builder()
                        .status(500)
                        .body(Body::from(format!("Error: {}", e)))
                        .unwrap()
                }
            }
        }
        Ok(None) => {
            let svg = render_no_flight_svg();
            match svg_to_dithered_png(&svg, &state.usvg_options) {
                Ok(png) => Response::builder()
                    .header("Content-Type", "image/png")
                    .header("Cache-Control", "no-cache, no-store, must-revalidate")
                    .body(Body::from(png))
                    .unwrap(),
                Err(e) => {
                    error!("Error rendering dithered PNG: {}", e);
                    Response::builder()
                        .status(500)
                        .body(Body::from(format!("Error: {}", e)))
                        .unwrap()
                }
            }
        }
        Err(e) => {
            error!("Error fetching flight: {}", e);
            Response::builder()
                .status(500)
                .body(Body::from(format!("Error: {}", e)))
                .unwrap()
        }
    }
}

async fn get_image_bin(State(state): State<AppState>) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let fetch_result = fetch_closest_flight().await;
    let fetch_duration = start.elapsed();

    match fetch_result {
        Ok(Some(flight)) => {
            let svg = render_svg(&flight);
            match svg_to_epd_bin(&svg, &state.usvg_options) {
                Ok(bin) => {
                    info!(
                        "Request processed (BIN): fetch={:?}, total={:?}",
                        fetch_duration,
                        start.elapsed()
                    );
                    Response::builder()
                        .header("Content-Type", "application/octet-stream")
                        .header("Cache-Control", "no-cache, no-store, must-revalidate")
                        .body(Body::from(bin))
                        .unwrap()
                }
                Err(e) => {
                    error!("Error rendering BIN: {}", e);
                    Response::builder()
                        .status(500)
                        .body(Body::from(format!("Error: {}", e)))
                        .unwrap()
                }
            }
        }
        Ok(None) => {
            let svg = render_no_flight_svg();
            match svg_to_epd_bin(&svg, &state.usvg_options) {
                Ok(bin) => Response::builder()
                    .header("Content-Type", "application/octet-stream")
                    .header("Cache-Control", "no-cache, no-store, must-revalidate")
                    .body(Body::from(bin))
                    .unwrap(),
                Err(e) => {
                    error!("Error rendering BIN: {}", e);
                    Response::builder()
                        .status(500)
                        .body(Body::from(format!("Error: {}", e)))
                        .unwrap()
                }
            }
        }
        Err(e) => {
            error!("Error fetching flight: {}", e);
            Response::builder()
                .status(500)
                .body(Body::from(format!("Error: {}", e)))
                .unwrap()
        }
    }
}

fn svg_to_epd_bin(svg: &str, opt: &usvg::Options) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let tree = Tree::from_str(svg, opt)?;
    let pixmap_size = tree.size();
    let mut pixmap = Pixmap::new(pixmap_size.width() as u32, pixmap_size.height() as u32).unwrap();
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());

    let dithered = apply_floyd_steinberg(pixmap);
    Ok(pixmap_to_epd_bin(dithered))
}

fn get_epd_color(rgb: [u8; 3]) -> u8 {
    if rgb == [0, 0, 0] {
        0
    }
    // BLACK
    else if rgb == [255, 255, 255] {
        1
    }
    // WHITE
    else if rgb == [255, 255, 0] {
        2
    }
    // YELLOW
    else if rgb == [255, 0, 0] {
        3
    }
    // RED
    else if rgb == [0, 0, 255] {
        5
    }
    // BLUE
    else if rgb == [0, 255, 0] {
        6
    }
    // GREEN
    else {
        1
    } // Default to WHITE
}

fn pixmap_to_epd_bin(pixmap: Pixmap) -> Vec<u8> {
    let src_w = pixmap.width() as usize;
    let src_h = pixmap.height() as usize;

    // The EPD is 1200x1600 total, split into two 600x1600 vertical strips.
    const TARGET_W: usize = 1200;
    const TARGET_H: usize = 1600;
    const HALF_W: usize = TARGET_W / 2;

    let mut buffer = vec![0u8; TARGET_W * TARGET_H / 2];
    let half_buffer_len = buffer.len() / 2;

    let pixels = pixmap.pixels();

    for y_new in 0..TARGET_H {
        for x_new in 0..TARGET_W {
            // Rotate 90 degrees clockwise to fit 1600x1200 landscape into 1200x1600 portrait
            // x_new = (src_h - 1) - y_old  => y_old = (src_h - 1) - x_new
            // y_new = x_old               => x_old = y_new
            let x_old = y_new;
            let y_old = (src_h - 1).saturating_sub(x_new);

            if x_old < src_w && y_old < src_h {
                let p_idx = y_old * src_w + x_old;
                let p = pixels[p_idx].demultiply();
                let color = get_epd_color([p.red(), p.green(), p.blue()]);

                let (tx, offset) = if x_new < HALF_W {
                    (x_new, 0)
                } else {
                    (x_new - HALF_W, half_buffer_len)
                };

                let pixel_idx = tx + y_new * HALF_W;
                let byte_pos = offset + pixel_idx / 2;

                if (pixel_idx & 1) == 0 {
                    buffer[byte_pos] |= color << 4;
                } else {
                    buffer[byte_pos] |= color;
                }
            }
        }
    }

    buffer
}

fn svg_to_dithered_png(
    svg: &str,
    opt: &usvg::Options,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let tree = Tree::from_str(svg, opt)?;
    let pixmap_size = tree.size();
    let mut pixmap = Pixmap::new(pixmap_size.width() as u32, pixmap_size.height() as u32).unwrap();
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());

    let dithered = apply_floyd_steinberg(pixmap);
    Ok(dithered.encode_png()?)
}

fn apply_floyd_steinberg(pixmap: Pixmap) -> Pixmap {
    let width = pixmap.width() as usize;
    let height = pixmap.height() as usize;
    let mut data = vec![[0.0f32; 3]; width * height];

    // Convert to f32 for dithering
    for (i, pixel) in pixmap.pixels().iter().enumerate() {
        data[i] = [
            pixel.red() as f32,
            pixel.green() as f32,
            pixel.blue() as f32,
        ];
    }

    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            let current_pixel = data[idx];

            // 1. CLAMP the value to valid RGB range.
            // This prevents "phantom" colors from appearing due to error explosion.
            let old_rgb = [
                current_pixel[0].clamp(0.0, 255.0),
                current_pixel[1].clamp(0.0, 255.0),
                current_pixel[2].clamp(0.0, 255.0),
            ];

            let new_rgb = find_closest_color(old_rgb);

            // Update the buffer with the final quantized color
            data[idx] = [new_rgb[0] as f32, new_rgb[1] as f32, new_rgb[2] as f32];

            // 2. Calculate error using the CLAMPED value.
            // If you use the unclamped value here, the artifacts will persist.
            let err = [
                old_rgb[0] - new_rgb[0] as f32,
                old_rgb[1] - new_rgb[1] as f32,
                old_rgb[2] - new_rgb[2] as f32,
            ];

            // Distribute error
            if x + 1 < width {
                distribute_error(&mut data[y * width + x + 1], err, 7.0 / 16.0);
            }
            if y + 1 < height {
                if x > 0 {
                    distribute_error(&mut data[(y + 1) * width + x - 1], err, 3.0 / 16.0);
                }
                distribute_error(&mut data[(y + 1) * width + x], err, 5.0 / 16.0);
                if x + 1 < width {
                    distribute_error(&mut data[(y + 1) * width + x + 1], err, 1.0 / 16.0);
                }
            }
        }
    }

    let mut out_pixmap = Pixmap::new(width as u32, height as u32).unwrap();
    let out_pixels = out_pixmap.pixels_mut();
    for (i, rgb) in data.iter().enumerate() {
        let r = rgb[0].clamp(0.0, 255.0) as u8;
        let g = rgb[1].clamp(0.0, 255.0) as u8;
        let b = rgb[2].clamp(0.0, 255.0) as u8;
        out_pixels[i] = tiny_skia::ColorU8::from_rgba(r, g, b, 255).premultiply();
    }

    out_pixmap
}

fn find_closest_color(rgb: [f32; 3]) -> [u8; 3] {
    let mut min_dist = f32::MAX;
    let mut closest = PALETTE[0];

    for color in PALETTE {
        let dist = (rgb[0] - color[0] as f32).powi(2)
            + (rgb[1] - color[1] as f32).powi(2)
            + (rgb[2] - color[2] as f32).powi(2);
        if dist < min_dist {
            min_dist = dist;
            closest = color;
        }
    }
    closest
}

fn distribute_error(pixel: &mut [f32; 3], err: [f32; 3], factor: f32) {
    pixel[0] += err[0] * factor;
    pixel[1] += err[1] * factor;
    pixel[2] += err[2] * factor;
}

fn svg_to_png(svg: &str, opt: &usvg::Options) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let tree = Tree::from_str(svg, opt)?;

    let pixmap_size = tree.size();
    let mut pixmap = Pixmap::new(pixmap_size.width() as u32, pixmap_size.height() as u32).unwrap();
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());

    Ok(pixmap.encode_png()?)
}

async fn fetch_closest_flight() -> Result<Option<Flight>, Box<dyn std::error::Error>> {
    let lamin = LAT - BOX_SIZE;
    let lamax = LAT + BOX_SIZE;
    let lomin = LON - BOX_SIZE;
    let lomax = LON + BOX_SIZE;

    let url = format!(
        "https://opensky-network.org/api/states/all?lamin={}&lomin={}&lamax={}&lomax={}",
        lamin, lomin, lamax, lomax
    );

    let client = reqwest::Client::new();
    info!("Fetching flights from OpenSky: {}", url);
    let resp: OpenSkyResponse = client.get(url).send().await?.json().await?;

    let states = match resp.states {
        Some(s) => s,
        None => return Ok(None),
    };

    let mut flights = Vec::new();
    for state in states {
        let icao24 = state[0].as_str().unwrap_or_default().to_string();
        let callsign = state[1].as_str().unwrap_or_default().trim().to_string();
        let longitude = state[5].as_f64();
        let latitude = state[6].as_f64();
        let baro_altitude = state[7].as_f64();

        if let (Some(lat), Some(lon)) = (latitude, longitude) {
            // Filter out flights above the altitude limit
            if let Some(alt) = baro_altitude {
                if alt > MAX_ALTITUDE_METERS {
                    continue;
                }
            }

            let distance = ((lat - LAT).powi(2) + (lon - LON).powi(2)).sqrt();
            flights.push(Flight {
                icao24,
                callsign,
                flight_number: None,
                aircraft_type: None,
                distance,
                photo_url: None,
                photo_base64: None,
                origin_iata: None,
                origin_name: None,
                dest_iata: None,
                dest_name: None,
            });
        }
    }

    flights.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap());

    if let Some(mut flight) = flights.first().cloned() {
        if let Some(url) = fetch_photo_url(&flight.icao24).await {
            flight.photo_url = Some(url.clone());
            // Fetch the image and convert to base64 for resvg
            info!("Fetching plane photo from: {}", url);
            if let Ok(resp) = client.get(url).send().await {
                if let Ok(bytes) = resp.bytes().await {
                    let b64 = general_purpose::STANDARD.encode(bytes);
                    flight.photo_base64 = Some(format!("data:image/jpeg;base64,{}", b64));
                }
            }
        }
        if let Some(route) = fetch_route(&flight.callsign).await {
            flight.origin_iata = Some(route.origin.iata_code);
            flight.origin_name = Some(route.origin.municipality);
            flight.dest_iata = Some(route.destination.iata_code);
            flight.dest_name = Some(route.destination.municipality);
            flight.flight_number = route.callsign_iata;
        }
        if let Some(aircraft) = fetch_aircraft_info(&flight.icao24).await {
            flight.aircraft_type = Some(aircraft.aircraft_type);
        }
        Ok(Some(flight))
    } else {
        Ok(None)
    }
}

async fn fetch_route(callsign: &str) -> Option<AdsbdbFlightRoute> {
    let url = format!("https://api.adsbdb.com/v0/callsign/{}", callsign);
    let client = reqwest::Client::new();
    info!("Fetching route for callsign {}: {}", callsign, url);
    let resp: AdsbdbResponse = client
        .get(url)
        .header("User-Agent", "Radar/0.1.0")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    resp.response.flightroute
}

async fn fetch_aircraft_info(icao24: &str) -> Option<AdsbdbAircraft> {
    let url = format!("https://api.adsbdb.com/v0/aircraft/{}", icao24);
    let client = reqwest::Client::new();
    info!("Fetching aircraft info for hex {}: {}", icao24, url);
    let resp: AdsbdbResponse = client
        .get(url)
        .header("User-Agent", "Radar/0.1.0")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    resp.response.aircraft
}

async fn fetch_photo_url(icao24: &str) -> Option<String> {
    let url = format!("https://api.planespotters.net/pub/photos/hex/{}", icao24);
    let client = reqwest::Client::new();
    info!("Fetching photo URL for hex {}: {}", icao24, url);
    let resp: PlanespottersResponse = client
        .get(url)
        .header("User-Agent", "Radar/0.1.0")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    resp.photos.first().map(|p| p.thumbnail_large.src.clone())
}

fn render_svg(flight: &Flight) -> String {
    let callsign = if flight.callsign.is_empty() {
        "Unknown"
    } else {
        &flight.callsign
    };

    let aircraft_type = flight.aircraft_type.as_deref().unwrap_or("Unknown");
    let flight_number = flight.flight_number.as_deref().unwrap_or("---");

    let origin_iata = flight.origin_iata.as_deref().unwrap_or("???");
    let origin_name = flight.origin_name.as_deref().unwrap_or("Unknown Origin");
    let dest_iata = flight.dest_iata.as_deref().unwrap_or("???");
    let dest_name = flight.dest_name.as_deref().unwrap_or("Unknown Destination");

    let photo_data = flight.photo_base64.as_deref().unwrap_or("");
    let has_photo = !photo_data.is_empty();

    let image_layer = if has_photo {
        format!(
            r#"<image id="bg" href="{}" width="1600" height="1200" preserveAspectRatio="xMidYMid meet" />"#,
            photo_data
        )
    } else {
        "".to_string()
    };

    format!(
        r#"<svg width='1600' height='1200' viewBox='0 0 1600 1200' xmlns='http://www.w3.org/2000/svg'>
  <rect width='1600' height='1200' fill='white' />
  {image_layer}

  <!-- Overlay Boxes -->
  <rect x='0' y='0' width='1600' height='160' fill='white' fill-opacity='1.0' />
  <rect x='0' y='1040' width='1600' height='160' fill='white' fill-opacity='1.0' />

  <!-- Route (Top) -->
  <g transform='translate(0, 105)'>
    <!-- Origin -->
    <g transform='translate(400, 0)'>
      <text x='0' y='0' font-family='Google Sans, sans-serif' font-size='100' text-anchor='middle' fill='#000000' font-weight='bold'>{origin_iata}</text>
      <text x='0' y='45' font-family='Google Sans, sans-serif' font-size='35' text-anchor='middle' fill='#000000'>{origin_name}</text>
    </g>

    <!-- Arrow -->
    <text x='800' y='0' font-family='Google Sans, sans-serif' font-size='80' text-anchor='middle' fill='#000000' font-weight='bold'>→</text>

    <!-- Destination -->
    <g transform='translate(1200, 0)'>
      <text x='0' y='0' font-family='Google Sans, sans-serif' font-size='100' text-anchor='middle' fill='#000000' font-weight='bold'>{dest_iata}</text>
      <text x='0' y='45' font-family='Google Sans, sans-serif' font-size='35' text-anchor='middle' fill='#000000'>{dest_name}</text>
    </g>
  </g>

  <!-- Info Row (Bottom) -->
  <g transform='translate(0, 1090)'>
    <!-- Callsign -->
    <g transform='translate(200, 0)'>
      <text x='0' y='0' font-family='Google Sans, sans-serif' font-size='40' text-anchor='middle' fill='#000000'>CALLSIGN</text>
      <text x='0' y='85' font-family='Google Sans, sans-serif' font-size='90' text-anchor='middle' fill='#000000' font-weight='bold'>{callsign}</text>
    </g>

    <!-- Flight Number -->
    <g transform='translate(800, 0)'>
      <text x='0' y='0' font-family='Google Sans, sans-serif' font-size='40' text-anchor='middle' fill='#000000'>FLIGHT</text>
      <text x='0' y='85' font-family='Google Sans, sans-serif' font-size='90' text-anchor='middle' fill='#000000' font-weight='bold'>{flight_number}</text>
    </g>

    <!-- Aircraft Type -->
    <g transform='translate(1400, 0)'>
      <text x='0' y='0' font-family='Google Sans, sans-serif' font-size='40' text-anchor='middle' fill='#000000'>AIRCRAFT TYPE</text>
      <text x='0' y='85' font-family='Google Sans, sans-serif' font-size='70' text-anchor='middle' fill='#000000' font-weight='bold'>{aircraft_type}</text>
    </g>
  </g>
</svg>"#,
        image_layer = image_layer,
        origin_iata = origin_iata,
        origin_name = origin_name,
        dest_iata = dest_iata,
        dest_name = dest_name,
        callsign = callsign,
        flight_number = flight_number,
        aircraft_type = aircraft_type
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_svg() {
        let flight = Flight {
            icao24: "test".to_string(),
            callsign: "TEST123".to_string(),
            flight_number: Some("LX123".to_string()),
            aircraft_type: Some("Airbus A320".to_string()),
            distance: 0.1,
            photo_url: Some("http://example.com/photo.jpg".to_string()),
            photo_base64: Some("data:image/jpeg;base64,VEVTVA==".to_string()),
            origin_iata: Some("WAW".to_string()),
            origin_name: Some("Warsaw".to_string()),
            dest_iata: Some("ZRH".to_string()),
            dest_name: Some("Zurich".to_string()),
        };
        let svg = render_svg(&flight);
        assert!(svg.contains("TEST123"));
        assert!(svg.contains("LX123"));
        assert!(svg.contains("WAW"));
        assert!(svg.contains("ZRH"));
        assert!(svg.contains("Airbus A320"));
        assert!(svg.contains("data:image/jpeg;base64,VEVTVA=="));
    }

    #[test]
    fn test_render_no_flight_svg() {
        let svg = render_no_flight_svg();
        assert!(svg.contains("No flights overhead"));
    }
}

fn render_no_flight_svg() -> String {
    r#"<svg width='1600' height='1200' viewBox='0 0 1600 1200' xmlns='http://www.w3.org/2000/svg'>
  <rect width='1600' height='1200' fill='white' />

  <text x='800' y='600' font-family='Google Sans, sans-serif' font-size='100' text-anchor='middle' fill='#000000'>No flights overhead</text>

</svg>"#.to_string()
}
