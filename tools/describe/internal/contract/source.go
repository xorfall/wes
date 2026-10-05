package contract

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"
)

func bounded(r io.Reader) ([]byte, error) {
	b, e := io.ReadAll(io.LimitReader(r, MaxInput+1))
	if e != nil {
		return nil, errors.New("OpenAPI source read failed")
	}
	if len(b) > MaxInput {
		return nil, errors.New("OpenAPI source exceeds 4 MiB")
	}
	return b, nil
}

// Read preserves the original bytes, does not crawl or upload, and never prints response bodies
// or credential-bearing URLs. A link is fetched only when explicitly supplied by the caller.
func Read(ctx context.Context, from string, stdin io.Reader) ([]byte, error) {
	if from == "-" || from == "" {
		return bounded(stdin)
	}
	if strings.HasPrefix(from, "http://") || strings.HasPrefix(from, "https://") {
		address, err := url.Parse(from)
		if err != nil || address.Host == "" || address.User != nil || address.Fragment != "" || address.RawQuery != "" {
			return nil, errors.New("OpenAPI source URL must be HTTP(S), without userinfo, query or fragment")
		}
		client := &http.Client{Timeout: 30 * time.Second, Transport: &http.Transport{Proxy: nil}, CheckRedirect: func(r *http.Request, via []*http.Request) error {
			if len(via) >= 5 || r.URL.Scheme != address.Scheme || r.URL.Host != address.Host || r.URL.User != nil || r.URL.RawQuery != "" {
				return errors.New("OpenAPI source redirect rejected")
			}
			return nil
		}}
		defer client.CloseIdleConnections()
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, from, nil)
		if err != nil {
			return nil, errors.New("invalid OpenAPI source URL")
		}
		req.Header.Set("Accept", "application/json, application/yaml, text/yaml")
		response, err := client.Do(req)
		if err != nil {
			return nil, errors.New("OpenAPI source fetch failed")
		}
		defer response.Body.Close()
		if response.StatusCode/100 != 2 {
			return nil, errors.New("OpenAPI source URL did not return success")
		}
		return bounded(response.Body)
	}
	f, err := os.Open(from)
	if err != nil {
		return nil, errors.New("OpenAPI source file could not be opened")
	}
	defer f.Close()
	return bounded(f)
}
