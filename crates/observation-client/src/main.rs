//! The Observation web app: Bevy with WebGL2 in the page's
//! `#observation-canvas`, egui, and [`ClientPlugins`](observation_client::ClientPlugins).

fn main() {
    observation_client::app(observation_client::default_plugins()).run();
}
