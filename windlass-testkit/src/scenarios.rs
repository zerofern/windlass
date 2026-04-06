use serde_json::{Value, json};

/// All stub mappings for the happy-path scenario (normal boot, everything works).
pub fn happy_path_qbit() -> Vec<Value> {
    vec![
        json!({
            "request": { "method": "POST", "url": "/api/v2/auth/login" },
            "response": {
                "status": 200,
                "body": "Ok.",
                "headers": { "Set-Cookie": "SID=integration_test_sid; Path=/" }
            }
        }),
        json!({
            "request": { "method": "GET", "url": "/api/v2/torrents/info" },
            "response": { "status": 200, "jsonBody": [] }
        }),
        json!({
            "request": { "method": "POST", "url": "/api/v2/app/setPreferences" },
            "response": { "status": 200, "body": "" }
        }),
    ]
}

pub fn happy_path_mam() -> Vec<Value> {
    vec![
        json!({
            "request": { "method": "GET", "urlPath": "/json/dynamicSeedbox.php" },
            "response": {
                "status": 200,
                "jsonBody": {
                    "Success": true, "msg": "No change",
                    "ip": "10.8.0.1", "ASN": 212_238, "AS": "Datacamp Limited"
                }
            }
        }),
        json!({
            "request": { "method": "GET", "urlPath": "/jsonLoad.php" },
            "response": {
                "status": 200,
                "jsonBody": { "connectable": "yes", "username": "BrightVoyage" }
            }
        }),
    ]
}

pub fn happy_path_gotify() -> Vec<Value> {
    vec![json!({
        "request": { "method": "POST", "url": "/message" },
        "response": { "status": 200, "jsonBody": { "id": 1 } }
    })]
}

pub fn qbit_auth_fail() -> Vec<Value> {
    vec![
        json!({
            "request": { "method": "POST", "url": "/api/v2/auth/login" },
            "response": { "status": 200, "body": "Fails." }
        }),
        // keep torrents and prefs in case they get called anyway
        json!({
            "request": { "method": "GET", "url": "/api/v2/torrents/info" },
            "response": { "status": 403, "body": "Forbidden" }
        }),
        json!({
            "request": { "method": "POST", "url": "/api/v2/app/setPreferences" },
            "response": { "status": 403, "body": "Forbidden" }
        }),
    ]
}

pub fn mam_rate_limit() -> Vec<Value> {
    vec![
        json!({
            "request": { "method": "GET", "urlPath": "/json/dynamicSeedbox.php" },
            "response": { "status": 429, "body": "Too Many Requests" }
        }),
        json!({
            "request": { "method": "GET", "urlPath": "/jsonLoad.php" },
            "response": { "status": 429, "body": "Too Many Requests" }
        }),
    ]
}
