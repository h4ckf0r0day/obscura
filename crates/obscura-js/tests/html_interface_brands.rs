use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

#[test]
fn html_interface_brands_follow_tag_namespace_and_real_prototypes() {
    let mut runtime = ObscuraJsRuntime::new();
    runtime.set_dom(obscura_dom::parse_html("<!doctype html><head><base href=/></head><body><table><tbody><tr><td>cell</td></tr></tbody></table></body>"));
    runtime.run_page_init();
    let result = runtime.evaluate(r#"(() => {
        const pairs = [['base','HTMLBaseElement'],['td','HTMLTableCellElement'],['th','HTMLTableCellElement'],
          ['tbody','HTMLTableSectionElement'],['thead','HTMLTableSectionElement'],['tfoot','HTMLTableSectionElement'],
          ['tr','HTMLTableRowElement'],['col','HTMLTableColElement'],['colgroup','HTMLTableColElement'],
          ['picture','HTMLPictureElement'],['source','HTMLSourceElement'],['title','HTMLTitleElement'],['time','HTMLTimeElement']];
        const failures=[];
        for (const [tag,name] of pairs) {
          const C=globalThis[name],element=document.createElement(tag),unrelated=document.createElement(tag==='div'?'span':'div');
          if(typeof C!=='function'||!(element instanceof C)||unrelated instanceof C||Object.getPrototypeOf(element)!==C.prototype
             ||Object.getPrototypeOf(C.prototype)!==HTMLElement.prototype||Object.prototype.toString.call(element)!=='[object '+name+']') failures.push(name);
          if(!(element.cloneNode() instanceof C)) failures.push('clone '+name);
          if(document.createElementNS('urn:other',tag) instanceof C) failures.push('namespace '+name);
          const prefixed=document.createElementNS('http://www.w3.org/1999/xhtml','x:'+tag);
          for(const node of [prefixed,prefixed.cloneNode()]) {
            if(!(node instanceof C)||node.localName!==tag||node.prefix!=='x'||node.tagName!==('x:'+tag).toUpperCase()) failures.push('prefix '+name);
          }
          const literal=document.createElement('x:'+tag);
          for(const node of [literal,literal.cloneNode()]) {
            if(node instanceof C||node.localName!=='x:'+tag||node.prefix!==null||node.tagName!==('x:'+tag).toUpperCase()) failures.push('literal colon '+name);
          }
          const upper=document.createElementNS('http://www.w3.org/1999/xhtml',tag.toUpperCase());
          if(upper instanceof C || upper.localName!==tag.toUpperCase() || upper.cloneNode() instanceof C) failures.push('case '+name);
          let illegal=false;try{new C();}catch(e){illegal=e instanceof TypeError;}if(!illegal)failures.push('constructor '+name);
          if(Object.getOwnPropertyDescriptor(globalThis,name).enumerable)failures.push('enumerable '+name);
        }
        const cell=document.querySelector('td');cell.id='brand-cache-cell';cell.marker='retained';
        const sameWrapper=document.getElementById('brand-cache-cell')===cell
          &&cell.parentNode.childNodes[0]===cell&&document.querySelector('td').marker==='retained';
        return [failures,sameWrapper,document.querySelector('base') instanceof HTMLBaseElement,
          document.querySelector('td') instanceof HTMLTableCellElement,HTMLBaseElement!==HTMLTableCellElement,
          HTMLBaseElement.prototype!==HTMLTableCellElement.prototype];
    })()"#).unwrap();
    assert_eq!(result,json!([[],true,true,true,true,true]));
}
