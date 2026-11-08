package main

import (
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// Carried over from the Rust tests: the router and the component status.

// testAuthConfig uses cheap PBKDF2 settings so tests stay fast; the
// verifier reads the iterations from the hash string.
func testAuthConfig(t *testing.T, oidcEnabled bool) *authConfig {
	t.Helper()
	text, err := hashPassword("secret", 1000)
	if err != nil {
		t.Fatal(err)
	}
	hash, err := parsePasswordHash(text)
	if err != nil {
		t.Fatal(err)
	}
	config := &authConfig{user: "ytdlp_web", passwordHash: hash, cookieSecret: []byte(strings.Repeat("\x07", 32)), cookieSecure: oidcEnabled}
	if oidcEnabled {
		config.oidc = testOIDCConfig()
	}
	return config
}

// testOIDCOnly is OIDC with no local password.
func testOIDCOnly(t *testing.T) *authConfig {
	t.Helper()
	config := testAuthConfig(t, true)
	*config = authConfig{user: config.user, cookieSecret: config.cookieSecret, cookieSecure: true, oidc: config.oidc}
	return config
}

func testOIDCConfig() *oidcConfig {
	return &oidcConfig{issuer: "https://identity.example.test", clientID: "ytdlp_web", clientSecret: "placeholder",
		redirectURL: "https://ytdlp-web.example.test/auth/oidc/callback"}
}

// testApp runs with a stand-in yt-dlp (a shell script) unless one is given.
func testApp(t *testing.T, oidcEnabled bool, script ...string) *app {
	t.Helper()
	dir := t.TempDir()
	st, err := openStore(filepath.Join(dir, "ytdlp_web.sqlite"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(st.close)
	program := filepath.Join(dir, "yt-dlp")
	body := `out=""; prev=""; for a in "$@"; do [ "$prev" = "-o" ] && out="$a"; prev="$a"; done
echo "[download]  50.0% of 1KiB"; printf 'video' > "$(dirname "$out")/Clip [x].mp4"`
	if len(script) > 0 {
		body = script[0]
	}
	os.WriteFile(program, []byte("#!/bin/sh\n"+body+"\n"), 0o755)
	downloads := filepath.Join(dir, "downloads")
	os.MkdirAll(downloads, 0o755)
	cfg := &config{downloadDir: downloads, ytdlp: program, maxActive: 1, jobTTL: time.Hour, downloadTimeout: time.Minute,
		auth: testAuthConfig(t, oidcEnabled)}
	return newApp(cfg, st)
}

type result struct {
	status int
	header http.Header
	body   string
}

func call(a *app, method, path, contentType, body string, header ...string) result {
	request := httptest.NewRequest(method, path, strings.NewReader(body))
	request.RemoteAddr = "192.0.2.10:1234"
	if contentType != "" {
		request.Header.Set("Content-Type", contentType)
	}
	for i := 0; i+1 < len(header); i += 2 {
		request.Header.Set(header[i], header[i+1])
	}
	recorder := httptest.NewRecorder()
	a.ServeHTTP(recorder, request)
	raw, _ := io.ReadAll(recorder.Result().Body)
	return result{recorder.Code, recorder.Result().Header, string(raw)}
}

const formType = "application/x-www-form-urlencoded"

func sessionCookieFrom(t *testing.T, r result) string {
	t.Helper()
	for _, value := range r.header.Values("Set-Cookie") {
		if strings.HasPrefix(value, "ytdlp_web_session=") {
			return strings.SplitN(value, ";", 2)[0]
		}
	}
	t.Fatal("no session cookie")
	return ""
}

func TestRepeatedWrongPasswordsLockTheAddressOut(t *testing.T) {
	a := testApp(t, true)
	for i := 0; i < 5; i++ {
		if r := call(a, "POST", "/login", formType, "password=wrong"); r.status != http.StatusOK {
			t.Fatalf("attempt %d: %d", i, r.status)
		}
	}
	if r := call(a, "POST", "/login", formType, "password=secret"); r.status != http.StatusTooManyRequests {
		t.Fatalf("locked login: %d", r.status)
	}
	if r := call(a, "GET", "/", "", "", "Authorization", "Basic eXRkbHBfd2ViOnNlY3JldA=="); r.status != http.StatusTooManyRequests {
		t.Fatalf("locked Basic: %d", r.status)
	}
}

func TestLoginRouteOffersOrganizationAndLocalBreakGlass(t *testing.T) {
	r := call(testApp(t, true), "GET", "/login", "", "")
	if r.status != http.StatusOK || !strings.Contains(r.body, "Organization login") || !strings.Contains(r.body, "Local login") {
		t.Fatalf("%d %q", r.status, r.body)
	}
}

func TestDownloadPagePreservesTheFormContract(t *testing.T) {
	r := call(testApp(t, true), "GET", "/", "", "", "Authorization", "Basic eXRkbHBfd2ViOnNlY3JldA==")
	if r.status != http.StatusOK {
		t.Fatal(r.status)
	}
	for _, want := range []string{`action="/downloads"`, "YTP Downloader", "Download Video", "https://youtu.be/..."} {
		if !strings.Contains(r.body, want) {
			t.Fatal(want)
		}
	}
}

func TestLocalBreakGlassLoginNeverContactsOIDC(t *testing.T) {
	r := call(testApp(t, true), "POST", "/login", formType, "password=secret")
	if r.status != http.StatusSeeOther {
		t.Fatal(r.status)
	}
	sessionCookieFrom(t, r)
}

func TestLogoutInvalidatesTheOpaqueSession(t *testing.T) {
	a := testApp(t, true)
	cookie := sessionCookieFrom(t, call(a, "POST", "/login", formType, "password=secret"))
	if r := call(a, "POST", "/logout", "", "", "Cookie", cookie); r.status != http.StatusSeeOther {
		t.Fatal(r.status)
	}
	r := call(a, "GET", "/", "", "", "Cookie", cookie)
	if r.status != http.StatusSeeOther || r.header.Get("Location") != "/login" {
		t.Fatalf("%d %q", r.status, r.header.Get("Location"))
	}
}

func TestCallbackRouteRejectsMissingFlowAndClearsFlowCookie(t *testing.T) {
	r := call(testApp(t, true), "GET", "/auth/oidc/callback", "", "")
	if r.status != http.StatusBadRequest {
		t.Fatal(r.status)
	}
	for _, value := range r.header.Values("Set-Cookie") {
		if strings.HasPrefix(value, "ytdlp_web_oidc_flow=") {
			if !strings.Contains(value, "Max-Age=0") || !strings.Contains(value, "Secure") {
				t.Fatal(value)
			}
			return
		}
	}
	t.Fatal("no flow cookie")
}

func TestEchoesOnlySafeCorrelationIDs(t *testing.T) {
	request := httptest.NewRequest("GET", "/", nil)
	request.Header.Set("X-Correlation-Id", "pilot:request-42")
	if got := requestCorrelationID(request); got != "pilot:request-42" {
		t.Fatal(got)
	}
	request.Header.Set("X-Correlation-Id", "unsafe/value")
	if got := requestCorrelationID(request); len(got) != 43 || strings.Trim(got, "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_") != "" {
		t.Fatal(got)
	}
}

func TestFallbackCorrelationIDsAreRandomAndOpaque(t *testing.T) {
	request := httptest.NewRequest("GET", "/", nil)
	first, second := requestCorrelationID(request), requestCorrelationID(request)
	if first == second || len(first) != 43 {
		t.Fatal(first, second)
	}
}

// New: the answers axum gave for refused input are kept.
func TestRejectionsAnswerAsAxumDid(t *testing.T) {
	a := testApp(t, false)
	cases := []struct {
		method, path, contentType, body string
		status                          int
		want                            string
	}{
		{"GET", "/nope", "", "", 404, ""},
		{"DELETE", "/", "", "", 405, ""},
		{"POST", "/downloads", "", "", 415, "Form requests must have `Content-Type: application/x-www-form-urlencoded`"},
		{"POST", "/downloads", formType, "x=1", 422, "Failed to deserialize form body: missing field `url`"},
		{"POST", "/downloads", formType, "url=" + strings.Repeat("a", maxBodyBytes), 413, "Failed to buffer the request body: length limit exceeded"},
	}
	for _, c := range cases {
		r := call(a, c.method, c.path, c.contentType, c.body)
		if r.status != c.status || r.body != c.want {
			t.Errorf("%s %s: %d %q", c.method, c.path, r.status, r.body)
		}
	}
}
