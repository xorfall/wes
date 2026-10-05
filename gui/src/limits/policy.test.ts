import {afterEach,expect,it,vi} from 'vitest';
import catalogue from '../../../packages/budgets/catalog.json';
import {decodeBudgets,initializeBudgets,budget,readBudgets,saveBudgets,UNSUPPORTED_HOST_NOTICE} from './policy';
import type {BudgetSnapshot} from './policy';
const snapshot=(values:Record<string,number>={}):BudgetSnapshot=>({scope:'application',revision:0,entries:catalogue.map(entry=>({...entry,unit:entry.unit as 'count'|'bytes'|'milliseconds',active:values[entry.id]??entry.default,saved:entry.default,editable:true,restart:true}))});
afterEach(()=>vi.unstubAllGlobals());
it('validates complete unique policies, not partial or overflow responses',()=>{
 const fixture=snapshot();expect(decodeBudgets(fixture)).toBe(fixture);
 for(const invalid of [{...fixture,revision:1.5},{...fixture,entries:fixture.entries.slice(1)},{...fixture,entries:[fixture.entries[0],...fixture.entries.slice(0,-1)]},{...fixture,entries:fixture.entries.map(e=>e.id==='ui.panes'?{...e,active:0}:e)}])expect(()=>decodeBudgets(invalid)).toThrow('Invalid');
});
it('bootstraps shared capacities and never activates saved next-launch values',async()=>{
 const fetcher=vi.fn().mockResolvedValue({ok:true,text:async()=>JSON.stringify(snapshot({'ui.panes':8,'ui.view.roots':96}))});vi.stubGlobal('fetch',fetcher);
 await initializeBudgets();expect(budget('ui.panes')).toBe(8);
 const {max_panes}=await import('../surface/split-model');expect(max_panes()).toBe(8);
 const {max_active_view_roots}=await import('../value-views/limits');expect(max_active_view_roots()).toBe(96);
 fetcher.mockResolvedValue({ok:true,text:async()=>JSON.stringify({...snapshot({'ui.panes':8}),revision:1,entries:snapshot({'ui.panes':8}).entries.map(e=>e.id==='ui.panes'?{...e,saved:12}:e)})});
 await saveBudgets(0,{'ui.panes':12});expect(fetcher).toHaveBeenLastCalledWith('/operating-budgets',expect.objectContaining({method:'PUT',body:'{"revision":0,"values":{"ui.panes":12}}'}));
 await readBudgets();expect(max_panes()).toBe(8);expect(max_active_view_roots()).toBe(96);
});
it('does not automatically repeat unconfirmed saves or accept unknown knobs',async()=>{
 const fetcher=vi.fn().mockRejectedValue(new Error('Synthetic connection closed'));vi.stubGlobal('fetch',fetcher);
 await expect(saveBudgets(0,{'ui.panes':8})).rejects.toThrow('connection closed');expect(fetcher).toHaveBeenCalledTimes(1);
 await expect(saveBudgets(0,{'protocol.version':2})).rejects.toThrow('Invalid');expect(fetcher).toHaveBeenCalledTimes(1);
});
it('should_ExplainDesktopOnlySettings_When_TheHostAnswersNotImplemented',async()=>{
 // Arrange
 const fetcher=vi.fn().mockResolvedValue({ok:false,status:501,text:async()=>''});vi.stubGlobal('fetch',fetcher);
 // Act
 const read=readBudgets();
 // Assert
 await expect(read).rejects.toThrow(UNSUPPORTED_HOST_NOTICE);
 expect(UNSUPPORTED_HOST_NOTICE).toContain('WesDesk desktop');
 expect(UNSUPPORTED_HOST_NOTICE).toContain('wes --serve');
 expect(UNSUPPORTED_HOST_NOTICE).toContain('startup/default budgets');
 expect(fetcher).toHaveBeenCalledTimes(1);
 expect(fetcher).not.toHaveBeenCalledWith('/operating-budgets',expect.objectContaining({method:'PUT'}));
 // Other failures keep their own status, not the desktop notice.
 fetcher.mockResolvedValue({ok:false,status:500,text:async()=>''});
 await expect(readBudgets()).rejects.toThrow('Could not load operating budgets (HTTP 500).');
});
