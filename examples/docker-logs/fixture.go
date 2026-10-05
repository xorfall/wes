// The acceptance checker builds this static program in its temporary build context.
package main

import (
	"fmt"
	"os"
	"time"
)

func main() {
	fmt.Fprintln(os.Stdout, "INFO ready — sentetik")
	fmt.Fprintln(os.Stdout, "ERROR synthetic_stdout")
	fmt.Fprintln(os.Stderr, "ERROR synthetic_stderr")
	fmt.Fprintln(os.Stdout, "WARN fixture")
	fmt.Fprintln(os.Stdout, "INFO done")
	if len(os.Args) == 2 && os.Args[1] == "--follow" {
		// Bounded fixture lifetime even if its checker is interrupted; cleanup removes our container.
		for n := 1; n <= 12000; n++ {
			level := "INFO"
			if n%25 == 0 {
				level = "ERROR"
			}
			fmt.Fprintf(os.Stdout, "%s synthetic_live %d\n", level, n)
			time.Sleep(10 * time.Millisecond)
		}
	}
}
