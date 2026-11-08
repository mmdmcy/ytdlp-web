package main

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/netip"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// YTDLP Web: a private yt-dlp download queue for a trusted network.
// Standard library only.

const version = "0.1.0"

const usage = "usage: ytdlp-web [serve|hash-password --stdin|audit-public|import-sqlite --from COPY]"

func main() {
	args := os.Args[1:]
	command := "serve"
	if len(args) > 0 {
		command = args[0]
	}
	var err error
	switch command {
	case "serve":
		err = serve()
	case "hash-password":
		err = hashPasswordCmd(args[1:])
	case "audit-public":
		var code int
		code, err = auditPublicCmd(args[1:])
		if err == nil && code != 0 {
			os.Exit(1)
		}
	case "import-sqlite":
		err = importSQLiteCmd(args[1:])
	default:
		fmt.Fprintln(os.Stderr, usage)
	}
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
		os.Exit(1)
	}
}

type config struct {
	bind            string
	db              string
	downloadDir     string
	ytdlp           string
	jsRuntime       string
	maxActive       int
	jobTTL          time.Duration
	downloadTimeout time.Duration
	auth            *authConfig
}

func envOr(name, fallback string) string {
	if value, ok := os.LookupEnv(name); ok {
		return value
	}
	return fallback
}

// envPositive is a whole number above zero, or the fallback.
func envPositive(name string, fallback int64) int64 {
	if n, err := strconv.ParseInt(os.Getenv(name), 10, 64); err == nil && n > 0 {
		return n
	}
	return fallback
}

func configFromEnv() (*config, error) {
	jsRuntime := ""
	if value := os.Getenv("YTDLP_WEB_JS_RUNTIME"); value != "" && strings.TrimSpace(value) != "" {
		if _, err := os.Stat(value); err == nil {
			jsRuntime = value
		}
	}
	ttlHours := envPositive("YTDLP_WEB_JOB_TTL_HOURS", 24)
	auth, err := authConfigFromEnv()
	if err != nil {
		return nil, err
	}
	return &config{
		bind:            envOr("YTDLP_WEB_BIND", "127.0.0.1:8790"),
		db:              envOr("YTDLP_WEB_DB", "data/ytdlp_web.sqlite"),
		downloadDir:     envOr("YTDLP_WEB_DOWNLOAD_DIR", "data/downloads"),
		ytdlp:           envOr("YTDLP_WEB_YTDLP", "yt-dlp"),
		jsRuntime:       jsRuntime,
		maxActive:       int(min(envPositive("YTDLP_WEB_MAX_ACTIVE", 1), 1<<20)),
		jobTTL:          time.Duration(min(ttlHours, 1<<31)) * time.Hour,
		downloadTimeout: time.Duration(min(envPositive("YTDLP_WEB_DOWNLOAD_TIMEOUT_SECS", 2*60*60), 1<<33)) * time.Second,
		auth:            auth,
	}, nil
}

type app struct {
	config  *config
	auth    *authConfig
	store   *store
	jobs    *jobs
	limiter loginLimiter
	routes  *router
}

func newApp(cfg *config, st *store) *app {
	a := &app{config: cfg, auth: cfg.auth, store: st,
		jobs: &jobs{byID: map[string]*job{}, slots: make(chan struct{}, cfg.maxActive)}}
	rt := &router{}
	plain := func(h func(*http.Request) *response) func(*http.Request, []string) *response {
		return func(r *http.Request, _ []string) *response { return h(r) }
	}
	type handlers = map[string]func(*http.Request, []string) *response
	rt.handle("/", handlers{"GET": plain(a.downloadsPage), "POST": plain(a.downloadCreate)})
	rt.handle("/login", handlers{"GET": plain(a.loginPage), "POST": plain(a.loginPost)})
	rt.handle("/logout", handlers{"POST": plain(a.logoutPost)})
	rt.handle("/auth/oidc/start", handlers{"GET": plain(a.oidcStart)})
	rt.handle("/auth/oidc/callback", handlers{"GET": plain(a.oidcCallback)})
	rt.handle("/.well-known/linuxmice/component", handlers{"GET": plain(a.componentStatus)})
	rt.handle("/downloads", handlers{"GET": plain(a.downloadsPage), "POST": plain(a.downloadCreate)})
	rt.handle("/downloads/jobs/{id}", handlers{"GET": a.downloadJobPage})
	rt.handle("/downloads/jobs/{id}/status", handlers{"GET": a.downloadJobStatus})
	rt.handle("/downloads/jobs/{id}/file", handlers{"GET": a.downloadFile})
	a.routes = rt
	return a
}

func logError(what string, err error) {
	fmt.Fprintf(os.Stderr, "%s failed: %v\n", what, err)
}

func (a *app) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	a.loginThrottle(r, func() *response { return a.routes.serve(r) }).write(w)
}

func serve() error {
	cfg, err := configFromEnv()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(cfg.downloadDir, 0o777); err != nil {
		return err
	}
	st, err := openStore(cfg.db)
	if err != nil {
		return err
	}
	defer st.close()
	bind, err := netip.ParseAddrPort(cfg.bind)
	if err != nil {
		return errors.New("invalid socket address syntax")
	}
	listener, err := net.Listen("tcp", bind.String())
	if err != nil {
		return err
	}
	fmt.Printf("YTDLP Web listening on http://%s\n", bind)
	server := &http.Server{Handler: newApp(cfg, st), ReadHeaderTimeout: 30 * time.Second}
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, syscall.SIGTERM, syscall.SIGINT)
	go func() {
		<-stop
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		server.Shutdown(ctx)
	}()
	if err := server.Serve(listener); err != nil && !errors.Is(err, http.ErrServerClosed) {
		return err
	}
	return nil
}
