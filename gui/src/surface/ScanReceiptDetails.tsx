import type { StoredValue } from "../protocol";
import { lineText, MonoLine } from "./MonoLine";
import { receiptHeadline, receiptRows, scanReceiptOf } from "./scan-receipt";
import "./record-progress.css";

/**
 * The analysis receipt under its own disclosure, beside — never instead of — the generic value view.
 * Rows wrap here: this is the place where everything the receipt says is readable whole.
 */
export function ScanReceiptDetails({ value, open = false }: { readonly value: StoredValue | undefined; readonly open?: boolean }) {
  const receipt = scanReceiptOf(value);
  if (!receipt) return null;
  const headline = receiptHeadline(receipt);
  return (
    <details className="scan-receipt" open={open}>
      <summary aria-label={lineText(headline)}><MonoLine segments={headline} className="scan-receipt-headline" /></summary>
      {receiptRows(receipt).map((row, at) => <MonoLine key={at} segments={row} className="scan-receipt-row" />)}
    </details>
  );
}
