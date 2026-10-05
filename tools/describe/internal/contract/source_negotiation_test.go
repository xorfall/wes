package contract

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestReadNegotiatesOpenAPIWithoutSecretsOrTruncation(t *testing.T) {
	for _, body := range []string{"openapi: 3.1.0\npaths: {}\n", `{"openapi":"3.0.3","paths":{}}`, "<html>fallback</html>"} {
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if !strings.Contains(r.Header.Get("Accept"), "application/yaml") || r.Header.Get("Authorization") != "" {
				t.Errorf("unexpected document headers")
			}
			io.WriteString(w, body)
		}))
		got, err := Read(context.Background(), server.URL, nil)
		server.Close()
		if err != nil || string(got) != body {
			t.Fatalf("%q %v", got, err)
		}
	}
}
