#[allow(unused)]
use log::{error, info, warn};

use serde_json::{Value, json};
use std::fs;
use std::process::exit;

use librespot::core::authentication::Credentials;
use librespot::core::session::Session;
use librespot::core::spotify_uri::SpotifyUri;

use librespot::playback::audio_backend;
use librespot::playback::config::{AudioFormat, PlayerConfig};
use librespot::playback::mixer::NoOpVolume;
use librespot::playback::player::Player;

use librespot_oauth::DeviceAuthClientBuilder;

#[cfg(debug_assertions)]
const DEBUGMODE: bool = true;
#[cfg(not(debug_assertions))]
const DEBUGMODE: bool = false;

pub const VERSION: &str = "2.2.0";

pub fn check(version_info: String) {
    println!("ok {}", version_info);

    #[allow(unused_mut)]
    let mut capabilities = json!({
        "autoplay": true,
        "debug": DEBUGMODE,
        "device-auth": true,
        "lms-auth": true,
        "no-ap-port": true,
        "oauth": true,
        "podcasts": true,
        "save-token": true,
        "temp-dir": true,
        "version": VERSION,
        "volume-normalisation": true,
        "zeroconf-port": true,
        "zeroconf-backends": available_zeroconf_backends()
    });

    #[cfg(feature = "passthrough-decoder")]
    if let Value::Object(map) = &mut capabilities {
        map.insert("ogg-direct".to_string(), json!(true));
    }

    println!("{}", capabilities);
    exit(0);
}

// inspired by examples/get_token.rs
pub async fn get_token(
    _client_id: Option<String>,
    save_token: Option<String>,
    last_credentials: Option<Credentials>,
    session: Session,
) {
    match last_credentials {
        Some(last_credentials) => match session.connect(last_credentials, true).await {
            Ok(()) => match session.login5().auth_token().await {
                Ok(token) => {
                    write_response(
                        json!({
                            "accessToken": token.access_token,
                            "expiresIn": token.expires_in,
                        }),
                        save_token,
                    );
                }
                Err(error) => {
                    error!("Failed to fetch token: {:?}", error);
                    write_response(
                        json!({
                            "error": "Failed to get access token."
                        }),
                        save_token,
                    );
                }
            },
            Err(error) => {
                error!("Failed to create session (get_token): {:?}", error);
                write_response(
                    json!({
                        "error": "Failed to create session or connect to servers."
                    }),
                    save_token,
                );
            }
        },
        None => {
            println!("Missing credentials");
        }
    }
}

fn write_response(json_token: Value, save_token: Option<String>) {
    if let Some(save_token) = save_token {
        fs::write(&save_token, json_token.to_string()).expect("Can't write token file");
    } else {
        println!("{}", json_token);
    }
}

// inspired by examples/play.rs
pub async fn play_track(
    track_id: String,
    start_position: u32,
    last_credentials: Option<Credentials>,
    player_config: PlayerConfig,
    session: Session,
) {
    match last_credentials {
        Some(last_credentials) => {
            let backend = audio_backend::find(None).unwrap();
            let audio_format = AudioFormat::default();

            let track = SpotifyUri::from_uri(
                track_id
                    .replace("spotty://", "spotify:track:")
                    .replace("://", ":")
                    .as_str(),
            );

            if let Err(error) = session.connect(last_credentials, false).await {
                error!("Failed to create session (play_track): {:?}", error);
                return;
            }

            match track {
                Ok(track) => {
                    let player =
                        Player::new(player_config, session, Box::new(NoOpVolume), move || {
                            backend(None, audio_format)
                        });

                    player.load(track, true, start_position);
                    player.await_end_of_track().await;
                }
                Err(error) => {
                    error!("Failed to get track: {:?}", error);
                }
            };
        }
        None => {
            println!("Missing credentials");
        }
    }
}

fn available_zeroconf_backends() -> Vec<&'static str> {
    librespot::discovery::BACKENDS
        .iter()
        .filter_map(|(id, launch_svc)| launch_svc.map(|_| *id))
        .collect()
}

/// Pick a zeroconf backend that will actually work on this system when none was requested.
///
/// librespot defaults to the first compiled-in backend (avahi, then dns-sd, then libmdns).
/// Avahi is only useful if its daemon and the system D-Bus are running: on piCorePlayer and
/// many containers they are not, and libmdns is the safer choice. If avahi-daemon *is*
/// running, libmdns would fight it over port 5353, so we hand the job to avahi instead.
pub fn default_zeroconf_backend() -> Option<&'static str> {
    let backends = available_zeroconf_backends();

    if backends.contains(&"avahi") {
        let avahi_running = ["/run/avahi-daemon/socket", "/var/run/avahi-daemon/socket"]
            .iter()
            .any(|p| std::path::Path::new(p).exists());
        let dbus_running = ["/run/dbus/system_bus_socket", "/var/run/dbus/system_bus_socket"]
            .iter()
            .any(|p| std::path::Path::new(p).exists());

        if avahi_running && dbus_running {
            return Some("avahi");
        }
    }

    // on macOS dns-sd talks to mDNSResponder, which is always there
    if backends.contains(&"dns-sd") && cfg!(target_os = "macos") {
        return Some("dns-sd");
    }

    if backends.contains(&"libmdns") {
        return Some("libmdns");
    }

    None
}

/// Log in with Spotify's device code flow: the user enters a short code at spotify.com/pair.
///
/// This uses Spotify's desktop client ID, so the resulting session yields reusable
/// credentials for playback, just like a login through Spotify Connect discovery would,
/// but without depending on mDNS reaching the user's phone.
pub async fn device_auth(client_id: String, code_file: Option<String>) -> Option<Credentials> {
    let client = match DeviceAuthClientBuilder::new(&client_id, crate::OAUTH_SCOPES.to_vec()).build() {
        Ok(client) => client,
        Err(e) => {
            error!("Failed to create device auth client: {e}");
            write_response(json!({ "error": e.to_string() }), code_file);
            return None;
        }
    };

    let auth = match client.request_device_code_async().await {
        Ok(auth) => auth,
        Err(e) => {
            error!("Failed to get device code: {e}");
            write_response(json!({ "error": e.to_string() }), code_file);
            return None;
        }
    };

    info!("To authorize Spotty, open {} and enter the code {}", auth.verification_uri(), auth.user_code());
    write_response(
        json!({
            "userCode": auth.user_code(),
            "verificationUri": auth.verification_uri(),
            "url": auth.url(),
        }),
        code_file.clone(),
    );

    match client.poll_for_token_async(&auth).await {
        Ok(token) => Some(Credentials::with_access_token(token.access_token)),
        Err(e) => {
            error!("Device authorization failed: {e}");
            write_response(json!({ "error": e.to_string() }), code_file);
            None
        }
    }
}
