// Command extract converts OpenAPI JSON/YAML into native Wes API contracts.
package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"wes-describe/internal/contract"
)

var diagnosticsJSON = flag.Bool("diagnostics-json", false, "emit bounded private diagnostics as JSON on failure")

func main() {
	if err := run(); err != nil {
		var failure *extractionFailure
		if *diagnosticsJSON && errors.As(err, &failure) && failure.report != nil {
			_ = json.NewEncoder(os.Stdout).Encode(failure.report)
		}
		fmt.Fprintln(os.Stderr, err)
		os.Exit(exitCode(err))
	}
}
func run() error {
	editable := flag.Bool("draft", false, "produce an editable native draft")
	provider := flag.String("provider", "", "provider identifier (required)")
	from := flag.String("from", "-", "OpenAPI JSON/YAML file, HTTP(S) URL or stdin")
	out := flag.String("out", "", "new output file; never overwritten (default stdout)")
	partial := flag.Bool("allow-partial", false, "permit explicitly reported unsupported operations")
	flag.Parse()
	if flag.NArg() != 0 {
		return errors.New("unexpected positional arguments")
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt)
	defer stop()
	input, err := contract.Read(ctx, *from, os.Stdin)
	if err != nil {
		return err
	}
	doc, err := contract.Extract(input, *provider, *partial)
	if err != nil {
		return &extractionFailure{21, err, reportFailure(doc, err)}
	}
	var body []byte
	if *editable {
		draft, e := contract.EditableFromDocument(doc)
		if e != nil {
			return &extractionFailure{22, e, reportFailure(doc, e)}
		}
		body, err = draft.Marshal()
	} else {
		body, err = doc.Marshal()
	}
	if err != nil {
		return &extractionFailure{22, err, reportFailure(doc, err)}
	}
	if *out == "" {
		_, err = os.Stdout.Write(body)
		return err
	}
	file, err := os.OpenFile(*out, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
	if err != nil {
		return errors.New("cannot create output; existing files are not overwritten")
	}
	_, err = file.Write(body)
	closeErr := file.Close()
	if err != nil {
		return err
	}
	return closeErr
}

type extractionFailure struct {
	code   int
	err    error
	report *failureReport
}

func (e *extractionFailure) Error() string { return e.err.Error() }
func (e *extractionFailure) Unwrap() error { return e.err }
func exitCode(err error) int {
	var failure *extractionFailure
	if errors.As(err, &failure) {
		return failure.code
	}
	return 1
}
