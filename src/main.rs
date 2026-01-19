use axum::{
    Router,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use std::net::SocketAddr;
use tracing::{error, info};

const LAT: f64 = 47.4197;
const LON: f64 = 8.4344;
const BOX_SIZE: f64 = 0.5; // Roughly 50km

#[derive(Debug, Deserialize)]
struct OpenSkyResponse {
    states: Option<Vec<Vec<serde_json::Value>>>,
}

#[derive(Debug, Clone)]
struct Flight {
    _icao24: String,
    callsign: String,
    origin_country: String,
    _longitude: f64,
    _latitude: f64,
    altitude: Option<f64>,
    velocity: Option<f64>,
    distance: f64,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let app = Router::new()
        .route("/", get(index))
        .route("/image.svg", get(get_image));

    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let addr: SocketAddr = format!("0.0.0.0:{}", port).parse().unwrap();
    info!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn index() -> Html<&'static str> {
    Html("<h1>Radar</h1><p>Go to <a href='/image.svg'>/image.svg</a></p>")
}

async fn get_image() -> impl IntoResponse {
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
        let origin_country = state[2].as_str().unwrap_or_default().to_string();
        let longitude = state[5].as_f64();
        let latitude = state[6].as_f64();
        let altitude = state[7].as_f64();
        let velocity = state[9].as_f64();

        if let (Some(lat), Some(lon)) = (latitude, longitude) {
            let distance = ((lat - LAT).powi(2) + (lon - LON).powi(2)).sqrt();
            flights.push(Flight {
                _icao24: icao24,
                callsign,
                origin_country,
                _longitude: lon,
                _latitude: lat,
                altitude,
                velocity,
                distance,
            });
        }
    }

    flights.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap());

    Ok(flights.first().cloned())
}

fn render_svg(flight: &Flight) -> String {
    let callsign = if flight.callsign.is_empty() {
        "Unknown"
    } else {
        &flight.callsign
    };

    let alt = flight.altitude.map(|a| format!("{:.0} ft", a * 3.28084)).unwrap_or_else(|| "N/A".to_string());
    let vel = flight.velocity.map(|v| format!("{:.0} km/h", v * 3.6)).unwrap_or_else(|| "N/A".to_string());

    format!(
        r#"<svg width='1600' height='1200' viewBox='0 0 1600 1200' xmlns='http://www.w3.org/2000/svg'>
  <rect width='1600' height='1200' fill='white' />

  <!-- Main Display Area -->
  <g transform='translate(800, 450)'>
    <!-- Callsign -->
    <text x='0' y='50' font-family='sans-serif' font-size='250' text-anchor='middle' fill='#2980b9' font-weight='bold'>{callsign}</text>
  </g>

  <!-- Info Row -->
  <g transform='translate(0, 850)'>
    <!-- Country -->
    <g transform='translate(266, 0)'>
      <text x='0' y='0' font-family='sans-serif' font-size='40' text-anchor='middle' fill='#7f8c8d'>ORIGIN</text>
      <text x='0' y='80' font-family='sans-serif' font-size='70' text-anchor='middle' fill='#2c3e50' font-weight='bold'>{origin_country}</text>
    </g>

    <!-- Altitude -->
    <g transform='translate(800, 0)'>
      <text x='0' y='0' font-family='sans-serif' font-size='40' text-anchor='middle' fill='#7f8c8d'>ALTITUDE</text>
      <text x='0' y='80' font-family='sans-serif' font-size='70' text-anchor='middle' fill='#2c3e50' font-weight='bold'>{alt}</text>
    </g>

    <!-- Speed -->
    <g transform='translate(1333, 0)'>
      <text x='0' y='0' font-family='sans-serif' font-size='40' text-anchor='middle' fill='#7f8c8d'>SPEED</text>
      <text x='0' y='80' font-family='sans-serif' font-size='70' text-anchor='middle' fill='#2c3e50' font-weight='bold'>{vel}</text>
    </g>
  </g>

  <!-- Decorative Line -->
  <rect x='100' y='750' width='1400' height='4' fill='#ecf0f1' />
</svg>"#,
        callsign = callsign,
        origin_country = flight.origin_country,
        alt = alt,
        vel = vel
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_svg() {
        let flight = Flight {
            _icao24: "test".to_string(),
            callsign: "TEST123".to_string(),
            origin_country: "Testland".to_string(),
            _longitude: 0.0,
            _latitude: 0.0,
            altitude: Some(10000.0),
            velocity: Some(250.0),
            distance: 0.1,
        };
        let svg = render_svg(&flight);
        assert!(svg.contains("TEST123"));
        assert!(svg.contains("Testland"));
        assert!(svg.contains("32808 ft")); // 10000 * 3.28084
        assert!(svg.contains("900 km/h")); // 250 * 3.6
        assert!(!svg.contains("Data from OpenSky Network"));
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
