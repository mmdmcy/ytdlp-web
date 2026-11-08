package main

import (
	"encoding/json"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// Carried over from the Rust tests: accepted URLs, cache identity, file
// names, progress parsing and the yt-dlp process rules; plus a whole job.

func TestAcceptsYouTubeDomains(t *testing.T) {
	for _, ok := range []string{"youtu.be/dQw4w9WgXcQ", "https://www.youtube.com/watch?v=dQw4w9WgXcQ", "youtube-nocookie.com/embed/x", "https://YOUTUBE.COM./x"} {
		if _, accepted := normalizeYouTubeURL(ok); !accepted {
			t.Errorf("refused %q", ok)
		}
	}
	for _, bad := range []string{"https://example.com/watch?v=dQw4w9WgXcQ", "", "ftp://youtube.com/x", "https://youtube.com.evil.example/"} {
		if _, accepted := normalizeYouTubeURL(bad); accepted {
			t.Errorf("accepted %q", bad)
		}
	}
	if got, _ := normalizeYouTubeURL("  youtu.be/x  "); got != "https://youtu.be/x" {
		t.Fatal(got)
	}
}

func TestStableDownloadIdentityAndFilenameContracts(t *testing.T) {
	for in, want := range map[string]string{
		"https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10": "youtube:dQw4w9WgXcQ",
		"https://youtu.be/dQw4w9WgXcQ":                     "youtube:dQw4w9WgXcQ",
		"https://example.com/x":                            "url:https://example.com/x",
	} {
		if got := cacheKeyForURL(in); got != want {
			t.Errorf("%s: %s", in, got)
		}
	}
	if got := sanitizeFilename("  video: title?.mp4  "); got != "video__title_.mp4" {
		t.Fatal(got)
	}
}

func TestProgressParserExtractsDownloadPercent(t *testing.T) {
	if got := progressFromLine("[download]  42.7% of 12.34MiB at 1.2MiB/s"); got != "42.7%" {
		t.Fatal(got)
	}
	if got := progressFromLine("not progress"); got != "" {
		t.Fatal(got)
	}
}

func shell(script string) *exec.Cmd { return exec.Command("sh", "-c", script) }

func TestChattyStderrDoesNotBlockAndItsTailIsReported(t *testing.T) {
	// 200 KiB on stderr exceeds the pipe buffer; an unread pipe would block
	// the child before it ever prints to stdout or exits.
	script := `i=0; while [ $i -lt 20000 ]; do echo "stderr-line-$i" >&2; i=$((i+1)); done; echo '[download]  50.0% of 1MiB'; echo 'ERROR: final' >&2; exit 3`
	var lines [][2]string
	done := make(chan error, 1)
	go func() {
		done <- run(shell(script), 20*time.Second, func(line, progress string) { lines = append(lines, [2]string{line, progress}) })
	}()
	var err error
	select {
	case err = <-done:
	case <-time.After(30 * time.Second):
		t.Fatal("run hung on a chatty stderr")
	}
	if err == nil || !strings.HasPrefix(err.Error(), "yt-dlp exited with code 3: ") || !strings.HasSuffix(err.Error(), "ERROR: final") || len(err.Error()) > stderrTailBytes+64 {
		t.Fatal(err)
	}
	if len(lines) != 1 || lines[0] != [2]string{"[download]  50.0% of 1MiB", "50.0%"} {
		t.Fatal(lines)
	}
}

func TestWallClockTimeoutKillsTheProcess(t *testing.T) {
	started := time.Now()
	err := run(shell("sleep 30"), 300*time.Millisecond, func(string, string) {})
	if err == nil || !strings.Contains(err.Error(), "stopped after 0 seconds") || time.Since(started) > 10*time.Second {
		t.Fatal(err)
	}
}

func TestSuccessfulRunReportsNothing(t *testing.T) {
	if err := run(shell("echo ok; exit 0"), 5*time.Second, func(string, string) {}); err != nil {
		t.Fatal(err)
	}
}

// New: a missing program is worded as the Rust service worded it.
func TestAMissingProgramIsWordedAsBefore(t *testing.T) {
	err := run(exec.Command("/nonexistent/yt-dlp"), time.Second, func(string, string) {})
	if err == nil || err.Error() != "Could not start yt-dlp: No such file or directory (os error 2)" {
		t.Fatal(err)
	}
}

func waitForStatus(t *testing.T, a *app, path, cookie string) job {
	t.Helper()
	for i := 0; i < 100; i++ {
		r := call(a, "GET", path+"/status", "", "", "Cookie", cookie)
		var j job
		json.Unmarshal([]byte(r.body), &j)
		if j.Status == "complete" || j.Status == "error" {
			return j
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Fatal("the job did not finish")
	return job{}
}

func TestAWholeDownloadAndItsCache(t *testing.T) {
	a := testApp(t, false)
	cookie := sessionCookieFrom(t, call(a, "POST", "/login", formType, "password=secret"))
	created := call(a, "POST", "/downloads", formType, "url=youtu.be%2Fabc", "Cookie", cookie)
	location := created.header.Get("Location")
	if created.status != http.StatusSeeOther || !strings.HasPrefix(location, "/downloads/jobs/") || len(location) != len("/downloads/jobs/")+12 {
		t.Fatal(created.status, location)
	}
	j := waitForStatus(t, a, location, cookie)
	if j.Status != "complete" || j.Progress != "100%" || *j.Filename != "Clip [x].mp4" || j.Log[len(j.Log)-1] != "Download ready" {
		t.Fatalf("%+v", j)
	}
	file := call(a, "GET", location+"/file", "", "", "Cookie", cookie)
	if file.status != http.StatusOK || file.body != "video" || file.header.Get("Content-Type") != "video/mp4" ||
		file.header.Get("Content-Disposition") != `attachment; filename="Clip__x_.mp4"` {
		t.Fatal(file)
	}
	// The same video by another link comes from the cache.
	again := call(a, "POST", "/downloads", formType, "url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3Dabc", "Cookie", cookie)
	cached := waitForStatus(t, a, again.header.Get("Location"), cookie)
	if cached.Log[0] != "Using cached file" || cached.CacheKey != "youtube:abc" {
		t.Fatalf("%+v", cached)
	}
	if page := call(a, "GET", "/", "", "", "Cookie", cookie); strings.Count(page.body, `class="job"`) != 2 {
		t.Fatal(page.body)
	}
	if r := call(a, "POST", "/downloads", formType, "url=https%3A%2F%2Fexample.com%2F", "Cookie", cookie); !strings.Contains(r.body, "Only YouTube links are accepted.") {
		t.Fatal(r.body)
	}
}

func TestOldDownloadsAreCleanedUp(t *testing.T) {
	a := testApp(t, false)
	dir := filepath.Join(a.config.downloadDir, "aaaaaaaaaaaa")
	os.MkdirAll(dir, 0o755)
	os.WriteFile(filepath.Join(dir, "old.mp4"), []byte("x"), 0o644)
	a.store.update(func(d *storeData) error {
		d.DownloadCache = append(d.DownloadCache, cacheEntry{CacheKey: "youtube:old", FilePath: filepath.Join(dir, "old.mp4"), Filename: "old.mp4", CreatedAt: rfc3339(time.Now().Add(-2 * time.Hour))})
		return nil
	})
	a.cleanupDownloads()
	if _, err := os.Stat(dir); !os.IsNotExist(err) || len(a.store.data.DownloadCache) != 0 {
		t.Fatal("old download kept", err)
	}
}

func TestAFileOutsideTheDownloadFolderIsRefused(t *testing.T) {
	a := testApp(t, false)
	outside := filepath.Join(t.TempDir(), "secret.txt")
	os.WriteFile(outside, []byte("no"), 0o600)
	j := cachedJob("https://youtu.be/x", "youtube:x", outside, "secret.txt")
	a.jobs.byID[j.ID] = j
	cookie := sessionCookieFrom(t, call(a, "POST", "/login", formType, "password=secret"))
	if r := call(a, "GET", "/downloads/jobs/"+j.ID+"/file", "", "", "Cookie", cookie); r.status != http.StatusForbidden {
		t.Fatal(r.status)
	}
	if within("/data/downloads2/x", "/data/downloads") || !within("/data/downloads/x", "/data/downloads") {
		t.Fatal("component-wise containment")
	}
}
