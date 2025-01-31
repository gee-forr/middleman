mod config;

use hyper::service::{make_service_fn, service_fn};
use hyper::Client;
use hyper::{Body, Request, Response, Server};
use hyper_rustls::ConfigBuilderExt;

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::str::FromStr;
use tokio::fs;
use tokio::io::AsyncWriteExt;

fn recording_exists(recording_name: &str) -> bool {
    Path::new(&recording_name).exists()
}

fn recording_name(folder: &str, path: &str, method: &str) -> String {
    format!("{}/{}/{}", folder, path, method)
}

async fn proxy_handler(
    config: config::Config,
    req: Request<Body>,
) -> Result<Response<Body>, hyper::Error> {
    // Prepare the TLS client config
    let tls = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_native_roots()
        .with_no_client_auth();
    // Prepare the HTTPS connector
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_http1()
        .build();
    let client = Client::builder().build::<_, hyper::Body>(https);

    let upstream = config.upstream;
    let method = req.method().clone();
    let path = req.uri().clone();

    let uri = format!("{}{}", upstream, &path);

    println!("request  for     {} {}", &method, &path);

    let recording_path = recording_name(&config.tapes, &req.uri().to_string(), &method.to_string());
    fs::create_dir_all(format!("{}/{}", &config.tapes, &req.uri().to_string()))
        .await
        .expect("Failed to create a tape directory");

    if recording_exists(&recording_path) {
        //let contents = fs::read_to_string(&recording_path).await.unwrap();
        let c: Vec<u8> = fs::read(&recording_path).await.unwrap();
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut resp = httparse::Response::new(&mut headers);

        resp.parse(&c).unwrap();

        let mut start_of_body = 0;

        for i in 0..c.len() {
            let bs = "\r\n".as_bytes();
            if i + 3 < c.len() {
                if c[i] == bs[0] && c[i + 1] == bs[1] && c[i + 2] == bs[0] && c[i + 3] == bs[1] {
                    start_of_body = i + 4;
                    break;
                }
            }
        }

        let mut res = Response::builder().status(resp.code.unwrap());
        for i in resp.headers {
            res = res.header(i.name, i.value);
        }
        let bs: Vec<u8> = c[start_of_body..].try_into().unwrap();
        let res = res.body(Body::from(bs)).unwrap();
        println!("playback for {} {} {}", resp.code.unwrap(), &method, &path);
        Ok(res)
    } else {
        if config.replay_only {
            println!("Not Impl for {} {} {}", 501, &method, &path);
            let mut resp = Response::builder().status(501);

            if req.headers().get("accept").is_some() {
                resp = resp.header("accept", req.headers().get("accept").unwrap());
            }
            return Ok(resp.body(Body::from("")).unwrap());
        }
        let mut outgoing_request = Request::builder()
            .method(method.clone())
            .uri(uri)
            .version(req.version());

        for (k, v) in req.headers() {
            outgoing_request = outgoing_request.header(k, v);
        }

        let outgoing_request = outgoing_request.body(req.into_body()).unwrap();

        let client_response = client.request(outgoing_request).await?;

        // Modify the response as needed
        let modified_response = client_response;

        let mut file = tokio::fs::File::create(&recording_path)
            .await
            .expect("Could not write to the tapes directory");

        let header = format!(
            "{:?} {} {}\r\n",
            &modified_response.version(),
            &modified_response.status().as_str(),
            &modified_response
                .status()
                .canonical_reason()
                .or(Some(""))
                .unwrap()
        );
        let _ = file.write_all(header.as_bytes()).await;
        for (name, value) in modified_response.headers() {
            let header = format!("{}:{}\r\n", name, value.to_str().unwrap());
            let _ = file.write_all(header.as_bytes()).await;
        }
        let _ = file.write_all("\r\n".as_bytes()).await;

        let (parts, body) = modified_response.into_parts();
        let bytes = hyper::body::to_bytes(body).await.unwrap();

        let _ = file.write_all(&bytes.clone()).await;

        println!(
            "record   for {} {} {}",
            &parts.status.as_u16(),
            &method,
            &path
        );
        Ok(Response::from_parts(parts, Body::from(bytes)))
    }
}

#[tokio::main]
async fn main() {
    let config = config::get_config().await;

    let ip =
        IpAddr::from_str(&config.bind).expect("Looks like you didn't provide a valid IP for bind");
    let addr = SocketAddr::new(ip, config.port);
    println!("Listening on {}", addr);

    let make_service = make_service_fn(move |_conn| {
        let config = config.clone();
        async move { Ok::<_, Infallible>(service_fn(move |req| proxy_handler(config.clone(), req))) }
    });

    let server = Server::bind(&addr).serve(make_service);

    // And run forever...
    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
}
