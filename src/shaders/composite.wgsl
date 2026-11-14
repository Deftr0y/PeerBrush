// Derived preview only. Quantize at the source depth after each engine operation.
struct Node { first:vec4<u32>, second:vec4<u32>, third:vec4<u32>, fourth:vec4<u32> }
struct Params { dims:vec4<u32>, place:vec4<u32>, tables:vec4<u32> }
struct Applied { pixel:vec4<u32>, visible:f32 }
@group(0) @binding(0) var<storage,read> tiles:array<u32>;
@group(0) @binding(1) var<storage,read> nodes:array<Node>;
@group(0) @binding(2) var<storage,read_write> output:array<u32>;
@group(0) @binding(3) var<uniform> params:Params;
@group(0) @binding(4) var<storage,read> lookup:array<u32>;
// Matches the Rust node encoding: the high half holds a one-based table offset.
const BLEND_IF_SHIFT:u32=16u;
const RANGED_CLIP_BASE:u32=5u;
var<private> uncertain:bool;
var<private> clip_coverage_updates:u32;
fn rgba(a:u32,b:u32)->vec4<u32> {return vec4<u32>(a&65535u,a>>16u,b&65535u,b>>16u);}
fn quantize(v:vec4<f32>,maximum:f32)->vec4<u32> {
    // Device contraction/rounding may cross a native half-sample boundary.
    // Flag the whole pixel for shared-engine repair before presenting it.
    let c=clamp(v,vec4<f32>(0.0),vec4<f32>(maximum));
    // Allow for accumulated f32 error when several paints restore clip coverage.
    let epsilon=select(0.001,0.125,maximum>255.0)
        + f32(clip_coverage_updates)*maximum*0.000000238418579;
    uncertain=uncertain || any(abs(fract(c)-vec4<f32>(0.5))<vec4<f32>(epsilon));
    return vec4<u32>(floor(clamp(v,vec4<f32>(0.0),vec4<f32>(maximum))+vec4<f32>(0.5)));
}
fn blend(d:f32,s:f32,mode:u32,maximum:f32)->f32 {
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
        case 16u: {return clamp(d+2.0*s-(1.0+1.0/maximum),0.0,1.0);}
        default: {return s;}
    }
}
fn contrast(d:u32,s:u32,mode:u32,m:u32)->u32 {
    let midpoint=(m+1u)/2u;
    switch mode {
        case 17u: {
            if s==0u || s==m {return s;}
            if s<midpoint {
                let ramp=(s*m+midpoint/2u)/midpoint;
                let darkened=max(d+ramp,m)-m;
                return (darkened*m+(ramp-1u)/2u)/ramp;
            }
            let span=m-midpoint;
            let divisor=m-((s-midpoint)*m+span/2u)/span;
            return min((d*m+divisor/2u)/divisor,m);
        }
        case 18u: {
            if s<midpoint {return min(d,2u*s);}
            return max(d,2u*(s-midpoint));
        }
        case 19u: {
            let threshold=m+select(0u,1u,s>=midpoint);
            return select(0u,m,d+s>=threshold);
        }
        default: {return s;}
    }
}
fn luma_rank(rgb:vec3<u32>)->u32 {
    return (30u*rgb.x+59u*rgb.y+11u*rgb.z+50u)/100u;
}
fn luma(rgb:vec3<f32>)->f32 {
    return dot(rgb,vec3<f32>(30.0,59.0,11.0))/100.0;
}
fn rgb_bounds(rgb:vec3<f32>)->vec2<f32> {
    return vec2<f32>(min(min(rgb.x,rgb.y),rgb.z),max(max(rgb.x,rgb.y),rgb.z));
}
fn saturation(rgb:vec3<f32>)->f32 {
    let limits=rgb_bounds(rgb);
    return limits.y-limits.x;
}
fn with_saturation(rgb:vec3<f32>,amount:f32)->vec3<f32> {
    let limits=rgb_bounds(rgb);
    if limits.x==limits.y {return vec3<f32>(0.0);}
    return (rgb-vec3<f32>(limits.x))*amount/(limits.y-limits.x);
}
fn with_luma(rgb:vec3<f32>,wanted:f32,maximum:f32)->vec3<f32> {
    let centered=rgb-vec3<f32>(luma(rgb));
    let limits=rgb_bounds(centered);
    var scale=1.0;
    if -limits.x>wanted {
        scale=wanted/-limits.x;
    } else if limits.y>maximum-wanted {
        scale=(maximum-wanted)/limits.y;
    }
    return clamp(vec3<f32>(wanted)+centered*scale,vec3<f32>(0.0),vec3<f32>(maximum));
}
fn blend_color(dst:vec3<u32>,src:vec3<u32>,mode:u32,maximum:f32)->vec3<f32> {
    let d=vec3<f32>(dst);
    let s=vec3<f32>(src);
    switch mode {
        case 20u: {return select(d,s,luma_rank(src)<luma_rank(dst));}
        case 21u: {return select(d,s,luma_rank(src)>luma_rank(dst));}
        case 22u: {return with_luma(with_saturation(s,saturation(d)),luma(d),maximum);}
        case 23u: {return with_luma(with_saturation(d,saturation(s)),luma(d),maximum);}
        case 24u: {return with_luma(s,luma(d),maximum);}
        case 25u: {return with_luma(d,luma(s),maximum);}
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
    var b:vec3<f32>;
    if mode>=17u && mode<=19u {
        let mixed=vec3<u32>(
            contrast(dst.x,src.x,mode,maximum),
            contrast(dst.y,src.y,mode,maximum),
            contrast(dst.z,src.z,mode,maximum),
        );
        b=vec3<f32>(mixed)/m;
    } else if mode>=20u && mode<=25u {
        b=blend_color(dst.xyz,src.xyz,mode,m)/m;
    } else {
        b=vec3<f32>(blend(d.x,s.x,mode,m),blend(d.y,s.y,mode,m),blend(d.z,s.z,mode,m));
    }
    let color=((1.0-sa)*da*d+sa*((1.0-da)*s+da*b))/alpha*m;
    return quantize(vec4<f32>(color,alpha*m),m);
}
fn adjust(a:vec4<u32>,b:vec4<u32>,t:f32,mode:u32,maximum:u32)->vec4<u32> {
    if mode==0u {let rgb=vec3<f32>(a.xyz)*(1.0-t)+vec3<f32>(b.xyz)*t;return vec4<u32>(quantize(vec4<f32>(rgb,0.0),f32(maximum)).xyz,a.w);}
    let c=compose(vec4<u32>(a.xyz,maximum),vec4<u32>(b.xyz,maximum),t,mode,maximum);
    return vec4<u32>(c.xyz,a.w);
}
fn nearest(n:u32,d:u32)->u32 {
    return (n+d/2u)/d;
}
// Compare q*d against n*65536+bias without forming a wide product.
fn ratio_exceeds(q:u32,d:u32,n:u32,bias:u32)->bool {
    let low=q*(d&65535u);
    let high=q*(d>>16u)+(low>>16u);
    let numerator_high=n+(bias>>16u);
    return high>numerator_high || (high==numerator_high && (low&65535u)>(bias&65535u));
}
fn fill_ratio(n:u32,d:u32,m:u32,bias:u32)->u32 {
    if m==255u {return min(m,(n*256u+bias)/d);}
    // The f32 estimate is within one integer in the clamped 16-bit range.
    // Correct either side with an exact comparison of the base-65536 digits.
    let q=u32(min(f32(m),(f32(n)*65536.0+f32(bias))/f32(d)));
    if q>0u && ratio_exceeds(q,d,n,bias) {return q-1u;}
    if q<m && !ratio_exceeds(q+1u,d,n,bias) {return q+1u;}
    return q;
}
fn fill_channel(d:u32,s:u32,mode:u32,fill:u32,m:u32)->u32 {
    switch mode {
        case 3u: {return m-fill_ratio((m-d)*255u,m*255u-(m-s)*fill,m,0u);}
        case 4u: {
            let faded=nearest((m-s)*fill,255u);
            return max(d,faded)-faded;
        }
        case 7u: {
            let divisor=(m+1u)*255u-s*fill;
            return fill_ratio(d*255u,divisor,m,divisor/2u);
        }
        case 8u: {return min(m,d+nearest(s*fill,255u));}
        case 12u: {return u32(abs(i32(d)-i32(nearest(s*fill,255u))));}
        case 16u: {
            let delta=(2*i32(s)-i32(m))*i32(fill);
            let faded=sign(delta)*i32(nearest(u32(abs(delta)),255u));
            return u32(clamp(i32(d)+faded-1,0,i32(m)));
        }
        case 17u: {
            let middle=(m+1u)/2u;
            if s<middle {
                let ramp=m-nearest((m-nearest(s*m,middle))*fill,255u);
                let darkened=max(d+ramp,m)-m;
                return (darkened*m+(ramp-1u)/2u)/ramp;
            }
            let divisor=m-nearest(nearest((s-middle)*m,m-middle)*fill,255u);
            return min(m,nearest(d*m,divisor));
        }
        case 19u: {
            let strength=nearest(fill*(m-1u),255u);
            let anchor=nearest((m-s)*strength,m);
            return min(m,nearest((max(d,anchor)-anchor)*m,m-strength));
        }
        default: {return d;}
    }
}
fn compose_fill(dst:vec4<u32>,src:vec4<u32>,opacity:f32,mode:u32,fill:u32,maximum:u32)->vec4<u32> {
    if fill==0u || opacity==0.0 || src.w==0u {
        return select(dst,vec4<u32>(0u),dst.w==0u);
    }
    let m=f32(maximum);
    let coverage=f32(src.w)/m*opacity;
    let da=f32(dst.w)/m;
    let source_only=coverage*(f32(fill)/255.0)*(1.0-da);
    let alpha=da+source_only;
    if alpha<=0.0 {return vec4<u32>(0u);}
    if dst.w==0u {return quantize(vec4<f32>(vec3<f32>(src.xyz),alpha*m),m);}
    let mixed=vec3<u32>(
        fill_channel(dst.x,src.x,mode,fill,maximum),
        fill_channel(dst.y,src.y,mode,fill,maximum),
        fill_channel(dst.z,src.z,mode,fill,maximum),
    );
    let color=(vec3<f32>(src.xyz)*source_only+vec3<f32>(mixed)*(coverage*da)+vec3<f32>(dst.xyz)*((1.0-coverage)*da))/alpha;
    return quantize(vec4<f32>(color,alpha*m),m);
}
fn mix_pixels(a:vec4<u32>,b:vec4<u32>,t:f32,maximum:u32)->vec4<u32> {
    let m=f32(maximum);let aa=f32(a.w)/m;let ba=f32(b.w)/m;let alpha=aa*(1.0-t)+ba*t;
    if alpha<=0.0 {return vec4<u32>(0u);}
    if a.w==b.w {return adjust(a,b,t,0u,maximum);}
    let rgb=(vec3<f32>(a.xyz)*aa*(1.0-t)+vec3<f32>(b.xyz)*ba*t)/alpha;
    return quantize(vec4<f32>(rgb,alpha*m),m);
}
// A split endpoint includes its edge sample. Compare in integers before division
// so a rounded reciprocal cannot flip a hard cutoff on a native sample.
fn range_ramp(value:u32,low:u32,high:u32,step:u32)->f32 {
    if value<low {return 0.0;}
    if value>=high {return 1.0;}
    return f32(value-low+step)/f32(high-low+step);
}
fn range_gate(rgb:vec3<u32>,maximum:u32,offset:u32)->f32 {
    let step=maximum/255u;
    let gray_scale=1000u*step;
    let gray=(299u*rgb.x+590u*rgb.y+111u*rgb.z+gray_scale/2u)/gray_scale;
    let values=vec4<u32>(gray*step,rgb);
    var gate=1.0;
    for(var channel=0u;channel<4u;channel+=1u) {
        let pair=lookup[offset+2u*channel];
        if pair==0xffff0000u {continue;}
        let endpoints=vec4<u32>(pair&255u,(pair>>8u)&255u,(pair>>16u)&255u,pair>>24u)*step;
        let value=values[channel];
        gate*=range_ramp(value,endpoints.x,endpoints.y,step)
            *range_ramp(maximum-value,maximum-endpoints.w,maximum-endpoints.z,step);
    }
    return gate;
}
fn sample_max(node:Node)->u32 {return node.fourth.y&65535u;}
fn keep_channels(before:vec4<u32>,after:vec4<u32>,excluded:u32,maximum:u32)->vec4<u32> {
    if excluded==7u {return before;}
    if excluded==0u || after.w==0u {return after;}
    var result=after;
    for(var channel=0u;channel<3u;channel+=1u) {
        if (excluded&(1u<<channel))!=0u {
            result[channel]=min((before[channel]*before.w+after.w/2u)/after.w,maximum);
        }
    }
    return result;
}
fn keep_clipped_channels(before:vec4<u32>,after:vec4<u32>,excluded:u32,maximum:u32,visible:f32,restored:f32)->vec4<u32> {
    if excluded==0u || restored==0.0 {return after;}
    var kept=before;
    if visible!=restored {
        kept=quantize(vec4<f32>(vec3<f32>(before.xyz)*visible/restored,0.0),f32(maximum));
    }
    return vec4<u32>(select(after.xyz,kept.xyz,(vec3<u32>(excluded)&vec3<u32>(1u,2u,4u))!=vec3<u32>(0u)),after.w);
}
fn gated_amount(a:vec4<u32>,b:vec4<u32>,t:f32,node:Node,backdrop_scale:f32)->f32 {
    let ranges=node.fourth.z>>BLEND_IF_SHIFT;
    if ranges==0u || t==0.0 {return t;}
    let offset=params.tables.y+ranges-1u;
    let maximum=sample_max(node);
    let source=range_gate(b.xyz,maximum,offset);
    if a.w==0u || source==0.0 {return t*source;}
    let alpha=f32(a.w)/f32(maximum)*backdrop_scale;
    return t*(source*(1.0-alpha+alpha*range_gate(a.xyz,maximum,offset+1u)));
}
fn compose_layer(a:vec4<u32>,b:vec4<u32>,t:f32,node:Node)->vec4<u32> {
    let maximum=sample_max(node);
    let mode=node.third.w;
    if node.fourth.z==0u || b.w==0u || t==0.0 {return compose(a,b,t,mode,maximum);}
    let fill=node.fourth.z&0xffffu;
    if fill!=0u {return compose_fill(a,b,t,mode,fill-1u,maximum);}
    return compose(a,b,t,mode,maximum);
}
fn amount_at(node:Node,id:vec2<u32>)->f32 {
    let opacity=bitcast<f32>(node.second.z);
    if node.third.z==0xffffffffu {return opacity;}
    let at=params.tables.x+node.third.z*params.dims.x*params.dims.y+id.y*params.dims.x+id.x;
    return opacity*bitcast<f32>(lookup[at]);
}
fn clip_fill_factor(node:Node)->f32 {
    let fill=node.fourth.z&0xffffu;
    if fill==0u {return 1.0;}
    return f32(fill-1u)/255.0;
}
fn apply(a:vec4<u32>,b:vec4<u32>,t:f32,node:Node,id:vec2<u32>,visible:f32,inherited:u32)->Applied {
    let mode=node.third.w;let maximum=sample_max(node);
    let excluded=inherited|(node.fourth.y>>16u);
    if excluded==7u {return Applied(a,visible);}
    let action=node.fourth.w;
    var backdrop_scale=1.0;
    if (action==1u || action==3u) && node.fourth.z>>BLEND_IF_SHIFT!=0u {
        let base=nodes[node.third.y];
        backdrop_scale=amount_at(base,id)*clip_fill_factor(base)*visible;
    }
    switch action {
        case 1u: {
            let amount=gated_amount(a,b,t,node,backdrop_scale);
            var restored=visible;
            if visible!=1.0 {
                let coverage=f32(b.w)/f32(maximum)*amount*clip_fill_factor(node);
                if coverage>0.0 {
                    restored=visible+(1.0-visible)*coverage;
                    clip_coverage_updates+=1u;
                }
            }
            let c=compose_layer(vec4<u32>(a.xyz,maximum),b,amount,node);
            let output=keep_clipped_channels(a,vec4<u32>(c.xyz,a.w),excluded,maximum,visible,restored);
            return Applied(output,restored);
        }
        case 2u: {
            let amount=gated_amount(a,b,t,node,1.0);
            if mode==0u {return Applied(keep_channels(a,mix_pixels(a,b,amount,maximum),excluded,maximum),visible);}
            return Applied(keep_channels(a,adjust(a,b,amount,mode,maximum),excluded,maximum),visible);
        }
        case 3u: {
            if visible==0.0 {return Applied(a,visible);}
            let output=adjust(a,b,gated_amount(a,b,t,node,backdrop_scale),mode,maximum);
            return Applied(keep_clipped_channels(a,output,excluded,maximum,visible,visible),visible);
        }
        case 4u: {return Applied(keep_channels(a,mix_pixels(a,b,t,maximum),excluded,maximum),visible);}
        default: {
            let output=compose_layer(a,b,gated_amount(a,b,t,node,1.0),node);
            return Applied(keep_channels(a,output,excluded,maximum),visible);
        }
    }
}
@compute @workgroup_size(8,8)
fn composite(@builtin(global_invocation_id) id:vec3<u32>) {
    if id.x>=params.dims.x || id.y>=params.dims.y {return;}
    uncertain=false;
    clip_coverage_updates=0u;
    let position=vec2<i32>(bitcast<i32>(lookup[params.place.w+id.x]),bitcast<i32>(lookup[params.place.w+params.dims.x+id.y]));
    var stack:array<vec4<u32>,33>;
    // A clipped unit keeps its original shape in stack[depth].w.
    var visible:array<f32,33>;
    var restrictions:array<u32,33>;
    var depth=0u;stack[0]=vec4<u32>(0u);
    visible[0]=1.0;
    restrictions[0]=0u;
    for(var i=0u;i<params.place.z;i=i+1u) {
        let node=nodes[i];let kind=node.first.x;
        if kind==2u || kind==4u {
            let pass_through=node.fourth.w==4u;
            let initial=select(vec4<u32>(0u),stack[depth],pass_through);
            let excluded=select(0u,restrictions[depth]|(node.fourth.y>>16u),pass_through);
            depth=depth+1u;stack[depth]=initial;visible[depth]=1.0;restrictions[depth]=excluded;continue;
        }
        let amount=amount_at(node,id.xy);
        if kind==3u || kind==5u {
            let group=stack[depth];
            let coverage=visible[depth];
            depth=depth-1u;
            if kind==5u && node.fourth.z>>BLEND_IF_SHIFT!=0u {
                let before=stack[depth];
                let output=compose_layer(before,group,amount*coverage,node);
                stack[depth]=keep_channels(before,output,restrictions[depth]|(node.fourth.y>>16u),sample_max(node));
            } else {
                let result=apply(stack[depth],group,amount,node,id.xy,visible[depth],restrictions[depth]);
                stack[depth]=result.pixel;
                visible[depth]=result.visible;
            }
            continue;
        }
        let local=position-vec2<i32>(bitcast<i32>(node.second.x),bitcast<i32>(node.second.y));
        var source=vec4<u32>(0u);
        if all(local>=vec2<i32>(0)) && local.x<i32(node.first.y) && local.y<i32(node.first.z) {
            if kind==1u {source=rgba(node.first.w,node.fourth.x);}else {
                let slot=lookup[node.second.w+u32(local.y)/256u*node.third.x+u32(local.x)/256u];
                if slot!=0xffffffffu {
                    let pixel=(u32(local.y)%256u)*256u+u32(local.x)%256u;
                    if sample_max(node)==255u {let v=tiles[slot*65536u+pixel];source=vec4<u32>(v&255u,(v>>8u)&255u,(v>>16u)&255u,v>>24u);}
                    else {let at=slot*65536u+pixel*2u;source=rgba(tiles[at],tiles[at+1u]);}
                }
            }
        }
        if node.fourth.w==RANGED_CLIP_BASE {
            let coverage=gated_amount(stack[depth-1u],source,1.0,nodes[node.third.y],1.0);
            visible[depth]=coverage;
            stack[depth]=select(source,vec4<u32>(0u,0u,0u,source.w),coverage==0.0);
        } else {
            let result=apply(stack[depth],source,amount,node,id.xy,visible[depth],restrictions[depth]);
            stack[depth]=result.pixel;
            visible[depth]=result.visible;
        }
    }
    let at=((params.place.y+id.y)*params.dims.z+params.place.x+id.x)*3u;
    output[at]=stack[0].x|(stack[0].y<<16u);output[at+1u]=stack[0].z|(stack[0].w<<16u);
    output[at+2u]=select(0u,1u,uncertain);
}
