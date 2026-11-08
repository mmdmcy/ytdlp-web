package main

import (
	"encoding/hex"
	"errors"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"
)

// The downloader: job state in memory, accepted YouTube URLs, cache
// identity, retention and the download cache records, and the handlers
// that use them.

// maxBodyBytes is the router's body limit.
const maxBodyBytes = 1024 * 1024

type job struct {
	ID        string    `json:"id"`
	URL       string    `json:"url"`
	CacheKey  string    `json:"cache_key"`
	CreatedAt time.Time `json:"-"`
	Created   string    `json:"created_at"`
	Status    string    `json:"status"`
	Progress  string    `json:"progress"`
	Filename  *string   `json:"filename"`
	FilePath  *string   `json:"file_path"`
	Error     *string   `json:"error"`
	Log       []string  `json:"log"`
}

func newJob(rawURL, cacheKey string) *job {
	now := time.Now()
	return &job{
		ID: hex.EncodeToString(randomBytes(6)), URL: rawURL, CacheKey: cacheKey,
		CreatedAt: now, Created: jsonTime(now), Status: "queued", Log: []string{},
	}
}

func cachedJob(rawURL, cacheKey, filePath, filename string) *job {
	j := newJob(rawURL, cacheKey)
	j.Status, j.Progress = "complete", "100%"
	j.FilePath, j.Filename = &filePath, &filename
	j.Log = append(j.Log, "Using cached file")
	return j
}

// jobs is the in-memory queue; the download slots bound how many yt-dlp
// runs go at once.
type jobs struct {
	mu    sync.Mutex
	byID  map[string]*job
	slots chan struct{}
}

func (q *jobs) get(id string) (job, bool) {
	q.mu.Lock()
	defer q.mu.Unlock()
	j, ok := q.byID[id]
	if !ok {
		return job{}, false
	}
	copied := *j
	copied.Log = slices.Clone(j.Log)
	return copied, true
}

func (q *jobs) edit(id string, fn func(*job)) {
	q.mu.Lock()
	defer q.mu.Unlock()
	if j, ok := q.byID[id]; ok {
		fn(j)
	}
}

// newest is up to n jobs, newest first.
func (q *jobs) newest(n int) []job {
	q.mu.Lock()
	defer q.mu.Unlock()
	var list []job
	for _, j := range q.byID {
		list = append(list, *j)
	}
	slices.SortFunc(list, func(a, b job) int { return b.CreatedAt.Compare(a.CreatedAt) })
	return list[:min(n, len(list))]
}

func pushLog(j *job, line string) {
	line = strings.TrimSpace(line)
	if line == "" {
		return
	}
	j.Log = append(j.Log, line)
	if len(j.Log) > 80 {
		j.Log = slices.Clone(j.Log[len(j.Log)-80:])
	}
}

// normalizeYouTubeURL accepts youtu.be, youtube.com, youtube-nocookie.com
// and *.youtube.com links over HTTP(S); a link without a scheme gets
// https://. The link is kept as typed (trimmed).
func normalizeYouTubeURL(raw string) (string, bool) {
	value := strings.TrimSpace(raw)
	if value == "" {
		return "", false
	}
	if !strings.Contains(value, "://") {
		value = "https://" + value
	}
	u, err := url.Parse(value)
	if err != nil {
		return "", false
	}
	scheme := strings.ToLower(u.Scheme)
	if scheme != "http" && scheme != "https" {
		return "", false
	}
	host := strings.TrimRight(strings.ToLower(u.Hostname()), ".")
	accepted := host == "youtu.be" || host == "youtube.com" || host == "youtube-nocookie.com" || strings.HasSuffix(host, ".youtube.com")
	return value, accepted
}

// cacheKeyForURL is youtube:VIDEO_ID for youtu.be/ID and youtube.com
// ?v=ID links, otherwise url:LINK.
func cacheKeyForURL(rawURL string) string {
	if u, err := url.Parse(rawURL); err == nil && u.Host != "" {
		host := strings.ToLower(u.Hostname())
		if host == "youtu.be" {
			first, _, _ := strings.Cut(strings.TrimPrefix(u.EscapedPath(), "/"), "/")
			return "youtube:" + first
		}
		if host == "youtube.com" || strings.HasSuffix(host, ".youtube.com") {
			for _, pair := range orderedPairs(u.RawQuery) {
				if pair[0] == "v" {
					return "youtube:" + pair[1]
				}
			}
		}
	}
	return "url:" + rawURL
}

// orderedPairs is form_urlencoded::parse in order.
func orderedPairs(raw string) [][2]string {
	var pairs [][2]string
	for _, piece := range strings.Split(raw, "&") {
		if piece == "" {
			continue
		}
		name, value, _ := strings.Cut(piece, "=")
		pairs = append(pairs, [2]string{
			lossyUTF8(percentDecode(strings.ReplaceAll(name, "+", " "))),
			lossyUTF8(percentDecode(strings.ReplaceAll(value, "+", " "))),
		})
	}
	return pairs
}

// sanitizeFilename keeps ASCII letters, digits, '.', '-' and '_', trims
// underscores from both ends and keeps 120 characters.
func sanitizeFilename(value string) string {
	var b strings.Builder
	for _, ch := range strings.TrimSpace(value) {
		if ch < 128 && (ch >= 'a' && ch <= 'z' || ch >= 'A' && ch <= 'Z' || ch >= '0' && ch <= '9' || ch == '.' || ch == '-' || ch == '_') {
			b.WriteRune(ch)
		} else {
			b.WriteByte('_')
		}
	}
	sanitized := strings.Trim(b.String(), "_")
	if sanitized == "" {
		return "download"
	}
	if len(sanitized) > 120 {
		sanitized = sanitized[:120]
	}
	return sanitized
}

// within is Rust's Path::starts_with: whole components only.
func within(path, root string) bool {
	return path == root || root == "/" || strings.HasPrefix(path, strings.TrimSuffix(root, "/")+"/")
}

// cachedFileForKey returns a cached download when its file still exists
// inside the download folder.
func (a *app) cachedFileForKey(cacheKey string) (string, string, bool) {
	var entry *cacheEntry
	a.store.view(func(d *storeData) {
		for _, e := range d.DownloadCache {
			if e.CacheKey == cacheKey {
				found := e
				entry = &found
			}
		}
	})
	if entry == nil {
		return "", "", false
	}
	path, err1 := filepath.EvalSymlinks(entry.FilePath)
	root, err2 := filepath.EvalSymlinks(a.config.downloadDir)
	if err1 != nil || err2 != nil {
		return "", "", false
	}
	path, _ = filepath.Abs(path)
	root, _ = filepath.Abs(root)
	if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() && within(path, root) {
		return path, entry.Filename, true
	}
	return "", "", false
}

func (a *app) recordCompletedDownload(cacheKey, file, filename, createdAt string) {
	err := a.store.update(func(d *storeData) error {
		d.DownloadCache = slices.DeleteFunc(d.DownloadCache, func(e cacheEntry) bool { return e.CacheKey == cacheKey })
		d.DownloadCache = append(d.DownloadCache, cacheEntry{CacheKey: cacheKey, FilePath: file, Filename: filename, CreatedAt: createdAt})
		return nil
	})
	if err != nil {
		logError("download cache", err)
	}
}

// cleanupDownloads forgets finished jobs and cache entries older than the
// job lifetime, and removes an expired download's folder.
func (a *app) cleanupDownloads() {
	cutoff := time.Now().Add(-a.config.jobTTL)
	a.jobs.mu.Lock()
	for id, j := range a.jobs.byID {
		if j.CreatedAt.Before(cutoff) && (j.Status == "complete" || j.Status == "error") {
			delete(a.jobs.byID, id)
		}
	}
	a.jobs.mu.Unlock()
	var expired []cacheEntry
	a.store.view(func(d *storeData) {
		for _, e := range d.DownloadCache {
			created, ok := parseRFC3339(e.CreatedAt)
			if ok && created.Before(cutoff) {
				expired = append(expired, e)
			}
		}
	})
	for _, e := range expired {
		key := e.CacheKey
		if err := a.store.update(func(d *storeData) error {
			d.DownloadCache = slices.DeleteFunc(d.DownloadCache, func(c cacheEntry) bool { return c.CacheKey == key })
			return nil
		}); err != nil {
			logError("download cache cleanup", err)
		}
		if parent := filepath.Dir(e.FilePath); parent != "." && parent != e.FilePath {
			os.RemoveAll(parent)
		}
	}
}

// parseRFC3339 reads the times the store keeps (chrono's to_rfc3339).
func parseRFC3339(text string) (time.Time, bool) {
	t, err := time.Parse(time.RFC3339Nano, text)
	return t, err == nil
}

func (a *app) startDownloadJob(id string) {
	go a.runDownloadJob(id)
}

func (a *app) runDownloadJob(id string) {
	a.jobs.edit(id, func(j *job) { j.Status, j.Progress = "queued", "" })
	a.jobs.slots <- struct{}{}
	defer func() { <-a.jobs.slots }()
	a.jobs.edit(id, func(j *job) {
		j.Status, j.Progress = "downloading", "starting"
		pushLog(j, "Starting download")
	})
	current, ok := a.jobs.get(id)
	if !ok {
		return
	}
	fail := func(message string) {
		a.jobs.edit(id, func(j *job) { j.Status, j.Error = "error", &message })
	}
	jobDir := filepath.Join(a.config.downloadDir, current.ID)
	if err := os.MkdirAll(jobDir, 0o777); err != nil {
		fail("Could not create download directory: " + rustIOError(err))
		return
	}
	file, err := download(a.config.ytdlp, a.config.jsRuntime, jobDir, current.URL, a.config.downloadTimeout, func(line, progress string) {
		a.jobs.edit(id, func(j *job) {
			if progress != "" {
				j.Progress = progress
			}
			pushLog(j, line)
		})
	})
	if err != nil {
		fail(err.Error())
		return
	}
	filename := filepath.Base(file)
	a.recordCompletedDownload(current.CacheKey, file, filename, rfc3339(time.Now()))
	a.jobs.edit(id, func(j *job) {
		j.Status, j.Progress = "complete", "100%"
		j.FilePath, j.Filename = &file, &filename
		pushLog(j, "Download ready")
	})
}

func (a *app) downloadsPage(r *http.Request) *response {
	if guard := a.pageGuard(r); guard != nil {
		return guard
	}
	return html(http.StatusOK, downloadsPage(a.jobs.newest(12)))
}

func (a *app) downloadCreate(r *http.Request) *response {
	form, rejection := readForm(r)
	if rejection != nil {
		return rejection
	}
	rawURL, rejection := form.required("url")
	if rejection != nil {
		return rejection
	}
	if guard := a.pageGuard(r); guard != nil {
		return guard
	}
	target, ok := normalizeYouTubeURL(rawURL)
	if !ok {
		return html(http.StatusOK, page("YTP Downloader", refusedURLBody))
	}
	a.cleanupDownloads()
	cacheKey := cacheKeyForURL(target)
	var j *job
	if path, filename, found := a.cachedFileForKey(cacheKey); found {
		j = cachedJob(target, cacheKey, path, filename)
	} else {
		j = newJob(target, cacheKey)
	}
	a.jobs.mu.Lock()
	a.jobs.byID[j.ID] = j
	a.jobs.mu.Unlock()
	if j.Status != "complete" {
		a.startDownloadJob(j.ID)
	}
	return redirect("/downloads/jobs/" + j.ID)
}

func (a *app) downloadJobPage(r *http.Request, params []string) *response {
	if guard := a.pageGuard(r); guard != nil {
		return guard
	}
	if _, ok := a.jobs.get(params[0]); !ok {
		return text(http.StatusNotFound, "unknown job")
	}
	return html(http.StatusOK, jobPage(params[0]))
}

func (a *app) downloadJobStatus(r *http.Request, params []string) *response {
	if guard := a.rawGuard(r); guard != nil {
		return guard
	}
	j, ok := a.jobs.get(params[0])
	if !ok {
		return text(http.StatusNotFound, "unknown job")
	}
	body := encodeJSON(j, false)
	return newResponse(http.StatusOK, "application/json", body[:len(body)-1])
}

func (a *app) downloadFile(r *http.Request, params []string) *response {
	if guard := a.rawGuard(r); guard != nil {
		return guard
	}
	j, ok := a.jobs.get(params[0])
	if !ok {
		return text(http.StatusNotFound, "unknown job")
	}
	if j.Status != "complete" {
		return text(http.StatusConflict, "not ready")
	}
	if j.FilePath == nil {
		return text(http.StatusNotFound, "missing file")
	}
	path, err := canonical(*j.FilePath)
	if err != nil {
		return text(http.StatusNotFound, "missing file")
	}
	root, err := canonical(a.config.downloadDir)
	if err != nil {
		return text(http.StatusInternalServerError, "bad download dir")
	}
	info, err := os.Stat(path)
	if !within(path, root) || err != nil || !info.Mode().IsRegular() {
		return text(http.StatusForbidden, "forbidden")
	}
	file, err := os.Open(path)
	if err != nil {
		return text(http.StatusNotFound, "missing file")
	}
	filename := filepath.Base(path)
	if j.Filename != nil {
		filename = *j.Filename
	}
	var ascii strings.Builder
	for _, ch := range sanitizeFilename(filename) {
		if ch < 128 && ch >= 32 && ch != 127 && ch != '"' {
			ascii.WriteRune(ch)
		}
	}
	name := ascii.String()
	if name == "" {
		name = "download.mp4"
	}
	res := newResponse(http.StatusOK, contentTypeFor(path), nil)
	res.header.Set("Content-Disposition", `attachment; filename="`+name+`"`)
	res.header.Set("Cache-Control", "no-store")
	res.stream = file
	return res
}

func canonical(path string) (string, error) {
	resolved, err := filepath.EvalSymlinks(path)
	if err != nil {
		return "", err
	}
	return filepath.Abs(resolved)
}

// mediaTypes is mime_guess's answer for the files yt-dlp writes here (the
// format choice asks for MP4) and a few neighbours; anything else is
// application/octet-stream.
var mediaTypes = map[string]string{
	"mp4": "video/mp4", "m4v": "video/x-m4v", "m4a": "audio/m4a", "webm": "video/webm",
	"mkv": "video/x-matroska", "mka": "audio/x-matroska", "mov": "video/quicktime",
	"mp3": "audio/mpeg", "opus": "audio/ogg", "ogg": "audio/ogg", "wav": "audio/wav",
	"flac": "audio/flac", "aac": "audio/aac", "3gp": "video/3gpp", "avi": "video/x-msvideo",
	"flv": "video/x-flv", "ts": "video/vnd.dlna.mpeg-tts", "json": "application/json",
	"txt": "text/plain", "vtt": "text/vtt", "srt": "application/x-subrip",
	"jpg": "image/jpeg", "jpeg": "image/jpeg", "png": "image/png", "webp": "image/webp",
}

func contentTypeFor(path string) string {
	if t, ok := mediaTypes[strings.ToLower(rustExtension(filepath.Base(path)))]; ok {
		return t
	}
	return "application/octet-stream"
}

var errNoFile = errors.New("Download finished but no file was found.")

// findDownloadedFile is the largest (then newest) file in the job folder
// that is not a sidecar or a partial download.
func findDownloadedFile(dir string) (string, bool) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return "", false
	}
	best, bestSize, bestTime := "", int64(-1), time.Time{}
	for _, entry := range entries {
		path := filepath.Join(dir, entry.Name())
		info, err := os.Stat(path)
		if err != nil || !info.Mode().IsRegular() {
			continue
		}
		switch rustExtension(entry.Name()) {
		case "json", "part", "ytdl", "temp", "tmp":
			continue
		}
		if strings.HasSuffix(path, ".part") {
			continue
		}
		if info.Size() > bestSize || (info.Size() == bestSize && !info.ModTime().Before(bestTime)) {
			best, bestSize, bestTime = path, info.Size(), info.ModTime()
		}
	}
	return best, best != ""
}
