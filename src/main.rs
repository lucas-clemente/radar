use axum::{
    Router,
    response::{Html, IntoResponse, Response},
    routing::get,
    body::Body,
    extract::State,
};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{error, info};
use usvg::{fontdb, Tree};
use tiny_skia::Pixmap;
use base64::{engine::general_purpose, Engine as _};

#[derive(Clone)]
struct AppState {
    usvg_options: Arc<usvg::Options<'static>>,
}

const LAT: f64 = 47.4197;
const LON: f64 = 8.4344;
const BOX_SIZE: f64 = 0.5; // Roughly 50km

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
}

#[derive(Debug, Deserialize)]
struct AdsbdbFlightRoute {
    origin: AdsbdbAirport,
    destination: AdsbdbAirport,
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
    altitude: Option<f64>,
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
    tracing_subscriber::fmt::init();

    let mut fontdb = fontdb::Database::new();
    fontdb.load_system_fonts();
    let mut usvg_options = usvg::Options::default();
    usvg_options.fontdb = Arc::new(fontdb);

    let state = AppState {
        usvg_options: Arc::new(usvg_options),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/image.svg", get(get_image))
        .route("/image.png", get(get_image_png))
        .with_state(state);

    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let addr: SocketAddr = format!("0.0.0.0:{}", port).parse().unwrap();
    info!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn index() -> Html<&'static str> {
    Html("<h1>Radar</h1><p>Go to <a href='/image.svg'>/image.svg</a></p>")
}

async fn get_image(_state: State<AppState>) -> impl IntoResponse {
    match fetch_closest_flight().await {
        Ok(Some(flight)) => {
            let svg = render_svg(&flight);
            Response::builder()
                .header("Content-Type", "image/svg+xml")
                .header("Cache-Control", "no-cache, no-store, must-revalidate")
                .body(svg)
                .unwrap()
        }
        Ok(None) => {
            let svg = render_no_flight_svg();
            Response::builder()
                .header("Content-Type", "image/svg+xml")
                .header("Cache-Control", "no-cache, no-store, must-revalidate")
                .body(svg)
                .unwrap()
        }
        Err(e) => {
            error!("Error fetching flight: {}", e);
            Response::builder()
                .status(500)
                .body(format!("Error: {}", e))
                .unwrap()
        }
    }
}

async fn get_image_png(State(state): State<AppState>) -> impl IntoResponse {
    match fetch_closest_flight().await {
        Ok(Some(flight)) => {
            let svg = render_svg(&flight);
            match svg_to_png(&svg, &state.usvg_options) {
                Ok(png) => Response::builder()
                    .header("Content-Type", "image/png")
                    .header("Cache-Control", "no-cache, no-store, must-revalidate")
                    .body(Body::from(png))
                    .unwrap(),
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
                Ok(png) => Response::builder()
                    .header("Content-Type", "image/png")
                    .header("Cache-Control", "no-cache, no-store, must-revalidate")
                    .body(Body::from(png))
                    .unwrap(),
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
            error!("Error fetching flight: {}", e);
            Response::builder()
                .status(500)
                .body(Body::from(format!("Error: {}", e)))
                .unwrap()
        }
    }
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
        let altitude = state[7].as_f64();

        if let (Some(lat), Some(lon)) = (latitude, longitude) {
            let distance = ((lat - LAT).powi(2) + (lon - LON).powi(2)).sqrt();
            flights.push(Flight {
                icao24,
                callsign,
                altitude,
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
        }
        Ok(Some(flight))
    } else {
        Ok(None)
    }
}

async fn fetch_route(callsign: &str) -> Option<AdsbdbFlightRoute> {
    let url = format!("https://api.adsbdb.com/v0/callsign/{}", callsign);
    let client = reqwest::Client::new();
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

async fn fetch_photo_url(icao24: &str) -> Option<String> {
    let url = format!("https://api.planespotters.net/pub/photos/hex/{}", icao24);
    let client = reqwest::Client::new();
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

    let alt = flight
        .altitude
        .map(|a| format!("{:.0} ft", a * 3.28084))
        .unwrap_or_else(|| "N/A".to_string());

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

    let blur_layer = if has_photo {
        r#"
  <g clip-path='url(#topBoxClip)'>
    <use href='#bg' filter='url(#blurEffect)' />
  </g>
  <g clip-path='url(#bottomBoxClip)'>
    <use href='#bg' filter='url(#blurEffect)' />
  </g>"#
            .to_string()
    } else {
        "".to_string()
    };

    format!(
        r#"<svg width='1600' height='1200' viewBox='0 0 1600 1200' xmlns='http://www.w3.org/2000/svg'>
  <defs>
    <filter id='blurEffect'>
      <feGaussianBlur stdDeviation='15' />
    </filter>
    <clipPath id='topBoxClip'>
      <rect x='100' y='20' width='1400' height='150' rx='30' />
    </clipPath>
    <clipPath id='bottomBoxClip'>
      <rect x='100' y='950' width='1400' height='230' rx='30' />
    </clipPath>
  </defs>

  <rect width='1600' height='1200' fill='white' />
  {image_layer}
  {blur_layer}

  <!-- Glass Overlay Boxes -->
  <rect x='100' y='20' width='1400' height='150' rx='30' fill='white' fill-opacity='0.4' />
  <rect x='100' y='950' width='1400' height='230' rx='30' fill='white' fill-opacity='0.4' />

  <!-- Route (Top) -->
  <g transform='translate(0, 100)'>
    <!-- Origin -->
    <g transform='translate(400, 0)'>
      <text x='0' y='0' font-family='sans-serif' font-size='100' text-anchor='middle' fill='#000000' font-weight='bold'>{origin_iata}</text>
      <text x='0' y='45' font-family='sans-serif' font-size='35' text-anchor='middle' fill='#000000'>{origin_name}</text>
    </g>

    <!-- Arrow -->
    <text x='800' y='0' font-family='sans-serif' font-size='80' text-anchor='middle' fill='#000000' font-weight='bold'>→</text>

    <!-- Destination -->
    <g transform='translate(1200, 0)'>
      <text x='0' y='0' font-family='sans-serif' font-size='100' text-anchor='middle' fill='#000000' font-weight='bold'>{dest_iata}</text>
      <text x='0' y='45' font-family='sans-serif' font-size='35' text-anchor='middle' fill='#000000'>{dest_name}</text>
    </g>
  </g>

  <!-- Info Row (Bottom) -->
  <g transform='translate(0, 1040)'>
    <!-- Callsign -->
    <g transform='translate(400, 0)'>
      <text x='0' y='0' font-family='sans-serif' font-size='40' text-anchor='middle' fill='#000000'>CALLSIGN</text>
      <text x='0' y='85' font-family='sans-serif' font-size='90' text-anchor='middle' fill='#000000' font-weight='bold'>{callsign}</text>
    </g>

    <!-- Altitude -->
    <g transform='translate(1200, 0)'>
      <text x='0' y='0' font-family='sans-serif' font-size='40' text-anchor='middle' fill='#000000'>ALTITUDE</text>
      <text x='0' y='85' font-family='sans-serif' font-size='90' text-anchor='middle' fill='#000000' font-weight='bold'>{alt}</text>
    </g>
  </g>
</svg>"#,
        image_layer = image_layer,
        blur_layer = blur_layer,
        origin_iata = origin_iata,
        origin_name = origin_name,
        dest_iata = dest_iata,
        dest_name = dest_name,
        callsign = callsign,
        alt = alt
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
            altitude: Some(10000.0),
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
        assert!(svg.contains("WAW"));
        assert!(svg.contains("ZRH"));
        assert!(svg.contains("32808 ft"));
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

  <text x='800' y='600' font-family='sans-serif' font-size='100' text-anchor='middle' fill='#7f8c8d'>No flights overhead</text>

</svg>"#.to_string()
}
