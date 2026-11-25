//! `check`: verifies the Google Photos part on the PC and reports each step:
//! open the link without login, read the whole album, pick a random photo,
//! download it already converted to 800×480 JPEG.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use rand::Rng;

use crate::config::Config;
use crate::net;

const WIDTH: u32 = 800;
const HEIGHT: u32 = 480;
/// Safety stop: 300 photos per page, so this is 30 000 photos.
const MAX_PAGES: usize = 100;

fn usage() -> ! {
    eprintln!(
        "usage: frogterm-pc check [<album-share-link> | --config FILE] [--out DIR] [--index N] [--pages N] [--save-page]\n\
         \n\
         <album-share-link>  e.g. https://photos.app.goo.gl/XXXX; default: album_url from --config\n\
         --config FILE       config file to take the link from (default: config.toml)\n\
         --out DIR           where to save the photo (default: out)\n\
         --index N           use photo N instead of a random one\n\
         --pages N           read at most N pages of 300 photos (default: all)\n\
         --save-page         also save the raw share page to DIR/album.html (contains the album key!)"
    );
    std::process::exit(2);
}

pub fn main(args: &[String]) -> Result<()> {
    let mut link = None;
    let mut config = PathBuf::from("config.toml");
    let mut out_dir = PathBuf::from("out");
    let mut index = None;
    let mut max_pages = MAX_PAGES;
    let mut save_page = false;
    let mut it = args.iter();
    let num = |v: Option<&String>| -> usize { v.and_then(|v| v.parse().ok()).unwrap_or_else(|| usage()) };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--index" => index = Some(num(it.next())),
            "--pages" => max_pages = num(it.next()).max(1),
            "--config" => config = it.next().unwrap_or_else(|| usage()).into(),
            "--out" => out_dir = it.next().unwrap_or_else(|| usage()).into(),
            "--save-page" => save_page = true,
            _ if link.is_none() && a.starts_with("http") => link = Some(a.clone()),
            _ => usage(),
        }
    }
    let link = match link {
        Some(l) => l,
        None => Config::load(&config)?.album_url,
    };
    fs::create_dir_all(&out_dir).with_context(|| format!("create {}", out_dir.display()))?;
    let agent = net::agent();

    if save_page {
        let p = out_dir.join("album.html");
        fs::write(&p, net::get_text(&agent, &link)?)?;
        println!("raw share page saved to {} (contains the album key, don't share it)", p.display());
    }

    println!("[1/3] Reading album (no login) ...");
    let t = Instant::now();
    let mut bytes = 0;
    let photos = net::fetch_album(&agent, &link, max_pages, |p| {
        bytes += p.bytes;
        let total = p.total.map(|t| format!(" (Google says album has {t})")).unwrap_or_default();
        println!(
            "      page {:>3}: +{:<3} photos, {:>4} KB, {:.1} s{total}",
            p.page,
            p.added,
            p.bytes / 1024,
            p.took.as_secs_f32()
        );
    })?;
    println!(
        "      album: {} photos, {:.1} MB downloaded, {:.1} s",
        photos.len(),
        bytes as f64 / 1e6,
        t.elapsed().as_secs_f32()
    );
    if photos.is_empty() {
        bail!("the album has no photos");
    }
    let portrait = photos.iter().filter(|p| p.height > p.width).count();
    println!("      orientation: {portrait} portrait, {} landscape/square", photos.len() - portrait);

    let idx = match index {
        Some(i) if i < photos.len() => i,
        Some(i) => bail!("--index {i} out of range (album has {} photos)", photos.len()),
        None => rand::thread_rng().gen_range(0..photos.len()),
    };
    let photo = &photos[idx];
    println!("[2/3] Random pick: photo #{idx} of {} ({} x {})", photos.len(), photo.width, photo.height);

    println!("[3/3] Downloading as {WIDTH}x{HEIGHT} JPEG ...");
    let t = Instant::now();
    let jpeg = net::download_photo(&agent, photo, WIDTH, HEIGHT)?;
    let img = image::load_from_memory_with_format(&jpeg.bytes, image::ImageFormat::Jpeg).context("decode JPEG")?;
    if (img.width(), img.height()) != (WIDTH, HEIGHT) {
        bail!("expected {WIDTH}x{HEIGHT}, got {}x{}", img.width(), img.height());
    }
    let path = out_dir.join(format!("photo_{idx}.jpg"));
    fs::write(&path, &jpeg.bytes)?;
    println!(
        "      {}, {} KB, {}x{}, {}, {:.1} s -> {}",
        jpeg.content_type,
        jpeg.bytes.len() / 1024,
        img.width(),
        img.height(),
        if jpeg.progressive { "PROGRESSIVE" } else { "baseline" },
        t.elapsed().as_secs_f32(),
        path.display()
    );
    println!("\nOK: whole album readable without login; random photo saved to {}", path.display());
    Ok(())
}
