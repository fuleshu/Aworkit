// @vitest-environment jsdom
import {cleanup,fireEvent,render,screen} from "@testing-library/react";
import {afterEach,expect,it,vi} from "vitest";
import {CompressionSettings} from "./CompressionSettings";
import type {ModelConfiguration} from "../configuration";
import {useState} from "react";
afterEach(cleanup);
it("edits a frozen policy without removing compaction or unrelated model settings",()=>{
 const change=vi.fn();const model:ModelConfiguration={id:"model.test",name:"Test",remoteId:"test",enabled:true,capabilities:["text","tools"],parameters:{},compaction:{auto:false,retainTokens:512}};
 render(<CompressionSettings model={model} onChange={change}/>);
 expect(screen.getByLabelText("Compression mode")).toHaveProperty("value","lossless");
 fireEvent.change(screen.getByLabelText("Compression mode"),{target:{value:"adaptive"}});
 expect(change).toHaveBeenLastCalledWith({...model,compaction:{...model.compaction,compression:{mode:"adaptive"}}});
 fireEvent.change(screen.getByLabelText("Always preserve text"),{target:{value:"CASE-739\ncompliance"}});
 expect(change.mock.lastCall?.[0].compaction.compression.protectedText).toEqual(["CASE-739","compliance"]);
 for(const input of document.querySelectorAll("input,select,textarea"))expect(input.getAttribute("title")).toBeTruthy();
});

it("retains a trailing newline while entering multiple protected literals",()=>{
 const initial:ModelConfiguration={id:"model.test",name:"Test",remoteId:"test",enabled:true,capabilities:["text"],parameters:{}};
 function Editor(){const [model,setModel]=useState(initial);return <CompressionSettings model={model} onChange={setModel}/>;}
 render(<Editor/>);
 const input=screen.getByLabelText("Always preserve text");
 fireEvent.change(input,{target:{value:"first\n"}});expect(input).toHaveProperty("value","first\n");
 fireEvent.change(input,{target:{value:"first\nsecond"}});expect(input).toHaveProperty("value","first\nsecond");
});
