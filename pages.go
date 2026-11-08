package main

import (
	_ "embed"
	"strings"
)

// The pages, byte for byte as the Rust service rendered them.

//go:embed page.css
var pageCSS string

const appName = "YTDLP Web"

var htmlEscaper = strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;", `"`, "&quot;")

func htmlEscape(value string) string { return htmlEscaper.Replace(value) }

func page(title, body string) string {
	return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1, viewport-fit=cover, interactive-widget=resizes-content">
<title>` + htmlEscape(title) + `</title>
<style>
` + pageCSS + `
</style>
</head>
<body>` + body + `</body>
</html>`
}

func loginBody(oidcEnabled bool, message string) string {
	organization := ""
	if oidcEnabled {
		organization = `<a class="button" href="/auth/oidc/start">Organization login</a>
  <p>Or use the local break-glass account:</p>`
	}
	errorHTML := ""
	if message != "" {
		errorHTML = `<p class="error">` + message + `</p>`
	}
	return `
<main class="login">
  <h1>` + appName + `</h1>
  ` + organization + `
  ` + errorHTML + `
  <form action="/login" method="post">
    <label>Local password</label>
    <input name="password" type="password" autocomplete="current-password" autofocus required>
    <button type="submit">Local login</button>
  </form>
</main>
`
}

func authErrorBody(message string) string {
	return `<main class="login"><h1>` + appName + `</h1><p class="error">` + message +
		`</p><p><a href="/login">Use local break-glass login</a></p></main>`
}

func downloadsPage(jobs []job) string {
	var list strings.Builder
	for i, j := range jobs {
		if i == 12 {
			break
		}
		label := j.Status
		if j.Filename != nil {
			label = *j.Filename
		}
		list.WriteString(`<a class="job" href="/downloads/jobs/` + htmlEscape(j.ID) + `"><strong>` + htmlEscape(label) + `</strong><span>` + htmlEscape(j.Progress) + `</span></a>`)
	}
	return page("YTP Downloader", `
<nav><a href="/">YTDLP Web</a><strong>YTP Downloader</strong><form action="/logout" method="post"><button class="ghost" type="submit">Log out</button></form></nav>
<main class="download">
  <form action="/downloads" method="post" class="download-form">
    <input name="url" type="url" inputmode="url" placeholder="https://youtu.be/..." required autofocus>
    <button type="submit">Download Video</button>
  </form>
  <section class="jobs">`+list.String()+`</section>
</main>
`)
}

const refusedURLBody = `<nav><a href="/">YTDLP Web</a><strong>YTP Downloader</strong></nav><main class="download"><p class="error">Only YouTube links are accepted.</p><form action="/downloads" method="post" class="download-form"><input name="url" type="url" inputmode="url" required autofocus><button type="submit">Download Video</button></form></main>`

func jobPage(id string) string {
	escaped := htmlEscape(id)
	return page("YTP Downloader", `
<nav><a href="/">YTDLP Web</a><a href="/downloads">YTP Downloader</a></nav>
<main class="download">
  <h1 id="state">Preparing</h1>
  <div class="bar"><div id="fill"></div></div>
  <p id="error" class="error"></p>
  <p id="ready" hidden><a class="button" id="file" href="/downloads/jobs/`+escaped+`/file">Save</a></p>
  <pre id="log"></pre>
</main>
<script>
const statusUrl = "/downloads/jobs/`+escaped+`/status";
function width(progress) {
  const match = String(progress || "").match(/([0-9.]+)%/);
  if (!match) return "8%";
  return Math.max(4, Math.min(100, Number(match[1]))) + "%";
}
async function poll() {
  const response = await fetch(statusUrl, {cache: "no-store"});
  if (!response.ok) return;
  const job = await response.json();
  document.getElementById("state").textContent = job.status + (job.progress ? " · " + job.progress : "");
  document.getElementById("fill").style.width = width(job.progress);
  document.getElementById("log").textContent = (job.log || []).join("\n");
  if (job.status === "error") {
    document.getElementById("error").textContent = job.error || "Download failed.";
    return;
  }
  if (job.status === "complete") {
    document.getElementById("ready").hidden = false;
    document.getElementById("fill").style.width = "100%";
    return;
  }
  setTimeout(poll, 1500);
}
poll();
</script>
`)
}
