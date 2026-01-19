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
use std::time::{Duration, Instant};
use tiny_skia::Pixmap;
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use usvg::{Tree, fontdb};

#[derive(Clone)]
struct OpenSkyToken {
    access_token: String,
    expires_at: Instant,
}

#[derive(Clone)]
struct AppState {
    usvg_options: Arc<usvg::Options<'static>>,
    client: reqwest::Client,
    opensky_client_id: Option<String>,
    opensky_client_secret: Option<String>,
    opensky_token: Arc<RwLock<Option<OpenSkyToken>>>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
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
const BOX_SIZE: f64 = 0.15; // Increased to ensure we cover 8km radius
const MAX_ALTITUDE_METERS: f64 = 6096.0; // 20,000 feet
const MAX_DISTANCE_KM: f64 = 8.0;

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

fn haversine_distance(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6371.0; // Earth's radius in km
    let d_lat = (lat2 - lat1).to_radians();
    let d_lon = (lon2 - lon1).to_radians();
    let a = (d_lat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (d_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    r * c
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

    let client = reqwest::Client::builder()
        .user_agent("Radar/0.1.0")
        .build()
        .unwrap();

    let opensky_client_id = std::env::var("OPENSKY_CLIENT_ID").ok();
    let opensky_client_secret = std::env::var("OPENSKY_CLIENT_SECRET").ok();

    if opensky_client_id.is_some() {
        info!("OpenSky OAuth2 credentials found.");
    } else {
        info!("OpenSky OAuth2 credentials not found, using anonymous requests.");
    }

    let state = AppState {
        usvg_options: Arc::new(usvg_options),
        client,
        opensky_client_id,
        opensky_client_secret,
        opensky_token: Arc::new(RwLock::new(None)),
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

async fn get_image(State(state): State<AppState>) -> impl IntoResponse {
    match fetch_svg(&state).await {
        Ok(svg) => make_response("image/svg+xml", svg),
        Err(resp) => resp,
    }
}

async fn get_image_png(State(state): State<AppState>) -> impl IntoResponse {
    let svg = match fetch_svg(&state).await {
        Ok(svg) => svg,
        Err(resp) => return resp,
    };

    handle_result(
        svg_to_png(&svg, &state.usvg_options),
        "image/png",
        "Error rendering PNG",
    )
}

async fn get_image_dithered_png(State(state): State<AppState>) -> impl IntoResponse {
    let svg = match fetch_svg(&state).await {
        Ok(svg) => svg,
        Err(resp) => return resp,
    };

    handle_result(
        svg_to_dithered_png(&svg, &state.usvg_options),
        "image/png",
        "Error rendering dithered PNG",
    )
}

async fn get_image_bin(State(state): State<AppState>) -> impl IntoResponse {
    let svg = match fetch_svg(&state).await {
        Ok(svg) => svg,
        Err(resp) => return resp,
    };

    handle_result(
        svg_to_epd_bin(&svg, &state.usvg_options),
        "application/octet-stream",
        "Error rendering BIN",
    )
}

fn make_response(content_type: &str, body: impl Into<Body>) -> Response {
    Response::builder()
        .header("Content-Type", content_type)
        .header("Cache-Control", "no-cache, no-store, must-revalidate")
        .body(body.into())
        .unwrap()
}

fn handle_result<T, E>(result: Result<T, E>, content_type: &str, error_msg: &str) -> Response
where
    T: Into<Body>,
    E: std::fmt::Display,
{
    match result {
        Ok(data) => make_response(content_type, data),
        Err(e) => {
            error!("{}: {}", error_msg, e);
            Response::builder()
                .status(500)
                .body(Body::from(format!("{}: {}", error_msg, e)))
                .unwrap()
        }
    }
}

async fn fetch_svg(state: &AppState) -> Result<String, Response> {
    let start = std::time::Instant::now();
    let token = get_opensky_token(state).await;
    let fetch_result = fetch_closest_flight(&state.client, token.as_deref()).await;
    let fetch_duration = start.elapsed();

    match fetch_result {
        Ok(Some(flight)) => {
            info!("Flight found: fetch={:?}", fetch_duration);
            Ok(render_svg(&flight))
        }
        Ok(None) => {
            info!("No flight found: fetch={:?}", fetch_duration);
            Ok(render_no_flight_svg())
        }
        Err(e) => {
            error!("Error fetching flight: {} (took {:?})", e, fetch_duration);
            Err(Response::builder()
                .status(500)
                .body(Body::from(format!("Error: {}", e)))
                .unwrap())
        }
    }
}

async fn get_opensky_token(state: &AppState) -> Option<String> {
    let client_id = state.opensky_client_id.as_ref()?;
    let client_secret = state.opensky_client_secret.as_ref()?;

    // Check if we have a valid cached token
    {
        let token_lock = state.opensky_token.read().await;
        if let Some(token) = token_lock.as_ref() {
            // Buffer of 60 seconds to avoid edge cases
            if token.expires_at > Instant::now() + Duration::from_secs(60) {
                return Some(token.access_token.clone());
            }
        }
    }

    // Otherwise, fetch a new one
    let mut token_lock = state.opensky_token.write().await;

    // Re-check in case another thread fetched it while we were waiting for the write lock
    if let Some(token) = token_lock.as_ref() {
        if token.expires_at > Instant::now() + Duration::from_secs(60) {
            return Some(token.access_token.clone());
        }
    }

    let url = "https://auth.opensky-network.org/auth/realms/opensky-network/protocol/openid-connect/token";
    let params = [
        ("grant_type", "client_credentials"),
        ("client_id", client_id),
        ("client_secret", client_secret),
    ];

    info!("Fetching new OpenSky OAuth2 token");
    match state.client.post(url).form(&params).send().await {
        Ok(resp) => match resp.json::<TokenResponse>().await {
            Ok(token_resp) => {
                let new_token = OpenSkyToken {
                    access_token: token_resp.access_token.clone(),
                    expires_at: Instant::now() + Duration::from_secs(token_resp.expires_in),
                };
                *token_lock = Some(new_token);
                Some(token_resp.access_token)
            }
            Err(e) => {
                error!("Error parsing OpenSky token response: {}", e);
                None
            }
        },
        Err(e) => {
            error!("Error fetching OpenSky token: {}", e);
            None
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
    match rgb {
        [0, 0, 0] => 0,       // BLACK
        [255, 255, 255] => 1, // WHITE
        [255, 255, 0] => 2,   // YELLOW
        [255, 0, 0] => 3,     // RED
        [0, 0, 255] => 5,     // BLUE
        [0, 255, 0] => 6,     // GREEN
        _ => 1,               // Default to WHITE
    }
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

async fn fetch_closest_flight(
    client: &reqwest::Client,
    token: Option<&str>,
) -> Result<Option<Flight>, Box<dyn std::error::Error>> {
    let lamin = LAT - BOX_SIZE;
    let lamax = LAT + BOX_SIZE;
    let lomin = LON - BOX_SIZE;
    let lomax = LON + BOX_SIZE;

    let url = format!(
        "https://opensky-network.org/api/states/all?lamin={}&lomin={}&lamax={}&lomax={}",
        lamin, lomin, lamax, lomax
    );

    info!("Fetching flights from OpenSky: {}", url);
    let mut rb = client.get(url);
    if let Some(t) = token {
        rb = rb.bearer_auth(t);
    }
    let resp: OpenSkyResponse = rb.send().await?.json().await?;

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

            let distance = haversine_distance(LAT, LON, lat, lon);
            if distance > MAX_DISTANCE_KM {
                continue;
            }

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
        if let Some(url) = fetch_photo_url(client, &flight.icao24).await {
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
        if let Some(route) = fetch_route(client, &flight.callsign).await {
            flight.origin_iata = Some(route.origin.iata_code);
            flight.origin_name = Some(route.origin.municipality);
            flight.dest_iata = Some(route.destination.iata_code);
            flight.dest_name = Some(route.destination.municipality);
            flight.flight_number = route.callsign_iata;
        }
        if let Some(aircraft) = fetch_aircraft_info(client, &flight.icao24).await {
            flight.aircraft_type = Some(aircraft.aircraft_type);
        }
        Ok(Some(flight))
    } else {
        Ok(None)
    }
}

async fn fetch_route(client: &reqwest::Client, callsign: &str) -> Option<AdsbdbFlightRoute> {
    let url = format!("https://api.adsbdb.com/v0/callsign/{}", callsign);
    info!("Fetching route for callsign {}: {}", callsign, url);
    let resp: AdsbdbResponse = client.get(url).send().await.ok()?.json().await.ok()?;

    resp.response.flightroute
}

async fn fetch_aircraft_info(client: &reqwest::Client, icao24: &str) -> Option<AdsbdbAircraft> {
    let url = format!("https://api.adsbdb.com/v0/aircraft/{}", icao24);
    info!("Fetching aircraft info for hex {}: {}", icao24, url);
    let resp: AdsbdbResponse = client.get(url).send().await.ok()?.json().await.ok()?;

    resp.response.aircraft
}

async fn fetch_photo_url(client: &reqwest::Client, icao24: &str) -> Option<String> {
    let url = format!("https://api.planespotters.net/pub/photos/hex/{}", icao24);
    info!("Fetching photo URL for hex {}: {}", icao24, url);
    let resp: PlanespottersResponse = client.get(url).send().await.ok()?.json().await.ok()?;

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
    fn test_haversine_distance() {
        // Distance between two points in Zurich
        let lat1 = 47.3769;
        let lon1 = 8.5417;
        let lat2 = 47.3780;
        let lon2 = 8.5400;
        let dist = haversine_distance(lat1, lon1, lat2, lon2);
        // Approx 0.17 km
        assert!(dist > 0.1 && dist < 0.3);
    }

    #[test]
    fn test_render_svg() {
        let flight = Flight {
            icao24: "test".to_string(),
            callsign: "TEST123".to_string(),
            flight_number: Some("LX123".to_string()),
            aircraft_type: Some("Airbus A320".to_string()),
            distance: 5.0,
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
        assert!(svg.contains("rect width='1600' height='1200' fill='white'"));
    }
}

fn render_no_flight_svg() -> String {
    r#"<svg width='1600' height='1200' viewBox='0 0 1600 1200' xmlns='http://www.w3.org/2000/svg'>
  <rect width='1600' height='1200' fill='white' />
</svg>"#
        .to_string()
}
