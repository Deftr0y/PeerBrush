// Derived preview only. Quantize at the source depth after each engine operation.
struct Node { first:vec4<u32>, second:vec4<u32>, third:vec4<u32>, fourth:vec4<u32> }
struct Params { dims:vec4<u32>, place:vec4<u32>, masks:vec4<u32> }
@group(0) @binding(0) var<storage,read> tiles:array<u32>;
@group(0) @binding(1) var<storage,read> nodes:array<Node>;
@group(0) @binding(2) var<storage,read_write> output:array<u32>;
@group(0) @binding(3) var<uniform> params:Params;
@group(0) @binding(4) var<storage,read> lookup:array<u32>;
var<private> uncertain:bool;
fn rgba(a:u32,b:u32)->vec4<u32> {return vec4<u32>(a&65535u,a>>16u,b&65535u,b>>16u);}
fn quantize(v:vec4<f32>,maximum:f32)->vec4<u32> {
    // Device contraction/rounding may cross a native half-sample boundary.
    // Flag the whole pixel for shared-engine repair before presenting it.
    let c=clamp(v,vec4<f32>(0.0),vec4<f32>(maximum));
    let epsilon=select(0.001,0.125,maximum>255.0);
    uncertain=uncertain || any(abs(fract(c)-vec4<f32>(0.5))<vec4<f32>(epsilon));
    return vec4<u32>(floor(clamp(v,vec4<f32>(0.0),vec4<f32>(maximum))+vec4<f32>(0.5)));
}
fn blend(d:f32,s:f32,mode:u32)->f32 {
    switch mode {
        case 1u: {return min(d,s);}
        case 2u: {return d*s;}
        case 3u: {if d>=1.0 {return 1.0;}if s<=0.0 {return 0.0;}return 1.0-min((1.0-d)/s,1.0);}
        case 4u: {return max(d+s-1.0,0.0);}
        case 5u: {return max(d,s);}
        case 6u: {return 1.0-(1.0-d)*(1.0-s);}
        case 7u: {if d==0.0 {return 0.0;}if s>=1.0 {return 1.0;}return min(d/(1.0-s),1.0);}
        case 8u: {return min(d+s,1.0);}
        case 9u: {if d<0.5 {return 2.0*s*d;}return 1.0-2.0*(1.0-s)*(1.0-d);}
        case 10u: {
            if s<=0.5 {return d-(1.0-2.0*s)*d*(1.0-d);}
            var g=sqrt(d);if d<=0.25 {g=((16.0*d-12.0)*d+4.0)*d;}
            return d+(2.0*s-1.0)*(g-d);
        }
        case 11u: {if s<0.5 {return 2.0*s*d;}return 1.0-2.0*(1.0-s)*(1.0-d);}
        case 12u: {return abs(d-s);}
        case 13u: {return d+s-2.0*d*s;}
        case 14u: {return max(d-s,0.0);}
        case 15u: {if d==0.0 {return 0.0;}if s==0.0 {return 1.0;}return min(d/s,1.0);}
        default: {return s;}
    }
}
fn compose(dst:vec4<u32>,src:vec4<u32>,opacity:f32,mode:u32,maximum:u32)->vec4<u32> {
    if src.w==0u || opacity==0.0 {if dst.w==0u {return vec4<u32>(0u);}return dst;}
    if opacity==1.0 && ((mode==0u && src.w==maximum) || dst.w==0u) {return src;}
    let m=f32(maximum);let sa=f32(src.w)/m*opacity;let da=f32(dst.w)/m;
    let alpha=sa+da*(1.0-sa);
    if alpha<=0.0 {return vec4<u32>(0u);}
    // Keep exact endpoints when a driver lowers division to a reciprocal.
    let s=select(vec3<f32>(src.xyz)/m,vec3<f32>(1.0),src.xyz==vec3<u32>(maximum));
    let d=select(vec3<f32>(dst.xyz)/m,vec3<f32>(1.0),dst.xyz==vec3<u32>(maximum));
    let b=vec3<f32>(blend(d.x,s.x,mode),blend(d.y,s.y,mode),blend(d.z,s.z,mode));
    let color=((1.0-sa)*da*d+sa*((1.0-da)*s+da*b))/alpha*m;
    return quantize(vec4<f32>(color,alpha*m),m);
}
fn adjust(a:vec4<u32>,b:vec4<u32>,t:f32,mode:u32,maximum:u32)->vec4<u32> {
    if mode==0u {let rgb=vec3<f32>(a.xyz)*(1.0-t)+vec3<f32>(b.xyz)*t;return vec4<u32>(quantize(vec4<f32>(rgb,0.0),f32(maximum)).xyz,a.w);}
    let c=compose(vec4<u32>(a.xyz,maximum),vec4<u32>(b.xyz,maximum),t,mode,maximum);
    return vec4<u32>(c.xyz,a.w);
}
fn mix_pixels(a:vec4<u32>,b:vec4<u32>,t:f32,maximum:u32)->vec4<u32> {
    let m=f32(maximum);let aa=f32(a.w)/m;let ba=f32(b.w)/m;let alpha=aa*(1.0-t)+ba*t;
    if alpha<=0.0 {return vec4<u32>(0u);}
    if a.w==b.w {return adjust(a,b,t,0u,maximum);}
    let rgb=(vec3<f32>(a.xyz)*aa*(1.0-t)+vec3<f32>(b.xyz)*ba*t)/alpha;
    return quantize(vec4<f32>(rgb,alpha*m),m);
}
fn apply(a:vec4<u32>,b:vec4<u32>,t:f32,node:Node)->vec4<u32> {
    let mode=node.third.w;let maximum=node.fourth.y;
    switch node.fourth.w {
        case 1u: {let c=compose(vec4<u32>(a.xyz,maximum),b,t,mode,maximum);return vec4<u32>(c.xyz,a.w);}
        case 2u: {if mode==0u {return mix_pixels(a,b,t,maximum);}return adjust(a,b,t,mode,maximum);}
        case 3u: {return adjust(a,b,t,mode,maximum);}
        case 4u: {return mix_pixels(a,b,t,maximum);}
        default: {return compose(a,b,t,mode,maximum);}
    }
}
@compute @workgroup_size(8,8)
fn composite(@builtin(global_invocation_id) id:vec3<u32>) {
    if id.x>=params.dims.x || id.y>=params.dims.y {return;}
    uncertain=false;
    let position=vec2<i32>(bitcast<i32>(lookup[params.place.w+id.x]),bitcast<i32>(lookup[params.place.w+params.dims.x+id.y]));
    var stack:array<vec4<u32>,33>;
    var depth=0u;stack[0]=vec4<u32>(0u);
    for(var i=0u;i<params.place.z;i=i+1u) {
        let node=nodes[i];let kind=node.first.x;
        if kind==2u || kind==4u {
            let initial=select(vec4<u32>(0u),stack[depth],node.fourth.w==4u);
            depth=depth+1u;stack[depth]=initial;continue;
        }
        var amount=bitcast<f32>(node.second.z);
        if node.third.z!=0xffffffffu {
            let at=params.masks.x+node.third.z*params.dims.x*params.dims.y+id.y*params.dims.x+id.x;
            amount=amount*bitcast<f32>(lookup[at]);
        }
        if kind==3u || kind==5u {let group=stack[depth];depth=depth-1u;stack[depth]=apply(stack[depth],group,amount,node);continue;}
        let local=position-vec2<i32>(bitcast<i32>(node.second.x),bitcast<i32>(node.second.y));
        var source=vec4<u32>(0u);
        if all(local>=vec2<i32>(0)) && local.x<i32(node.first.y) && local.y<i32(node.first.z) {
            if kind==1u {source=rgba(node.first.w,node.fourth.x);}else {
                let slot=lookup[node.second.w+u32(local.y)/256u*node.third.x+u32(local.x)/256u];
                if slot!=0xffffffffu {
                    let pixel=(u32(local.y)%256u)*256u+u32(local.x)%256u;
                    if node.fourth.y==255u {let v=tiles[slot*65536u+pixel];source=vec4<u32>(v&255u,(v>>8u)&255u,(v>>16u)&255u,v>>24u);}
                    else {let at=slot*65536u+pixel*2u;source=rgba(tiles[at],tiles[at+1u]);}
                }
            }
        }
        stack[depth]=apply(stack[depth],source,amount,node);
    }
    let at=((params.place.y+id.y)*params.dims.z+params.place.x+id.x)*3u;
    output[at]=stack[0].x|(stack[0].y<<16u);output[at+1u]=stack[0].z|(stack[0].w<<16u);
    output[at+2u]=select(0u,1u,uncertain);
}
