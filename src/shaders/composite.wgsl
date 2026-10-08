// Preview-only byte compositing. Native16 and unsupported structures use the CPU engine.
struct Node { first:vec4<u32>, second:vec4<u32>, third:vec4<u32> }
struct Params { dims:vec4<u32>, place:vec4<u32> }
@group(0) @binding(0) var<storage,read> tiles:array<u32>;
@group(0) @binding(1) var<storage,read> nodes:array<Node>;
@group(0) @binding(2) var<storage,read_write> output:array<u32>;
@group(0) @binding(3) var<uniform> params:Params;
@group(0) @binding(4) var<storage,read> lookup:array<u32>;
fn rgba(v:u32)->vec4<u32> {return vec4<u32>(v&255u,(v>>8u)&255u,(v>>16u)&255u,v>>24u);}
fn packed(v:vec4<u32>)->u32 {return v.x|(v.y<<8u)|(v.z<<16u)|(v.w<<24u);}
fn compose(dst:vec4<u32>,src:vec4<u32>,opacity:f32)->vec4<u32> {
    if src.w==0u || opacity==0.0 {if dst.w==0u {return vec4<u32>(0u);}return dst;}
    if opacity==1.0 && (src.w==255u || dst.w==0u) {return src;}
    let sa=f32(src.w)/255.0*opacity;let da=f32(dst.w)/255.0;
    let alpha=sa+da*(1.0-sa);
    if alpha<=0.0 {return vec4<u32>(0u);}
    let s=vec3<f32>(src.xyz)/255.0;let d=vec3<f32>(dst.xyz)/255.0;
    // Preserve the CPU expression/order and quantize after each shared layer operation.
    let color=((1.0-sa)*da*d+sa*((1.0-da)*s+da*s))/alpha*255.0;
    return vec4<u32>(vec3<u32>(floor(clamp(color,vec3<f32>(0.0),vec3<f32>(255.0))+vec3<f32>(0.5))),u32(floor(alpha*255.0+0.5)));
}
@compute @workgroup_size(8,8)
fn composite(@builtin(global_invocation_id) id:vec3<u32>) {
    if id.x>=params.dims.x || id.y>=params.dims.y {return;}
    let position=vec2<i32>(bitcast<i32>(lookup[params.place.w+id.x]),bitcast<i32>(lookup[params.place.w+params.dims.x+id.y]));
    var stack:array<vec4<u32>,17>;
    var depth=0u;stack[0]=vec4<u32>(0u);
    for(var i=0u;i<params.place.z;i=i+1u) {
        let node=nodes[i];let kind=node.first.x;
        if kind==2u {depth=depth+1u;stack[depth]=vec4<u32>(0u);continue;}
        let opacity=bitcast<f32>(node.second.z);
        if kind==3u {let group=stack[depth];depth=depth-1u;stack[depth]=compose(stack[depth],group,opacity);continue;}
        let local=position-vec2<i32>(bitcast<i32>(node.second.x),bitcast<i32>(node.second.y));
        var source=vec4<u32>(0u);
        if all(local>=vec2<i32>(0)) && local.x<i32(node.first.y) && local.y<i32(node.first.z) {
            if kind==1u {source=rgba(node.first.w);}else {
                let slot=lookup[node.second.w+u32(local.y)/256u*node.third.x+u32(local.x)/256u];
                if slot!=0xffffffffu {source=rgba(tiles[slot*65536u+(u32(local.y)%256u)*256u+u32(local.x)%256u]);}
            }
        }
        stack[depth]=compose(stack[depth],source,opacity);
    }
    output[(params.place.y+id.y)*params.dims.z+params.place.x+id.x]=packed(stack[0]);
}
