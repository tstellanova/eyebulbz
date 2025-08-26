extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, LitStr, Expr};

/// Usage:
/// ```ignore
/// let poly = include_polyline!("./img/heart_poly.csv", Rgb565::RED, 2);
/// poly.draw(&mut display).unwrap();
/// ```
#[proc_macro]
pub fn include_polyline(input: TokenStream) -> TokenStream {
    // Expect three arguments: path, color expr, stroke_width expr
    let args: syn::punctuated::Punctuated<syn::Expr, syn::Token![,]> =
        syn::parse_macro_input!(input with syn::punctuated::Punctuated::parse_terminated);

    if args.len() != 3 {
        return syn::Error::new_spanned(
            args.first().unwrap(),
            "expected: include_polyline!(\"path.csv\", color, stroke_width)"
        )
        .to_compile_error()
        .into();
    }

    // First arg = CSV file path
    let path_lit: LitStr = match &args[0] {
        Expr::Lit(expr_lit) => {
            if let syn::Lit::Str(s) = &expr_lit.lit {
                s.clone()
            } else {
                panic!("First argument must be a string literal path to CSV file")
            }
        }
        _ => panic!("First argument must be a string literal path to CSV file"),
    };
    let path = path_lit.value();

    // Read CSV file contents at compile time
    let data = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", path, e));

    // Parse lines into Points
    let mut points = Vec::new();
    for (lineno, line) in data.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() != 2 {
            panic!("Invalid line {} in {}: '{}'", lineno + 1, path, line);
        }
        let x: f32 = parts[0].trim().parse()
            .unwrap_or_else(|_| panic!("Bad x at line {}", lineno + 1));
        let y: f32 = parts[1].trim().parse()
            .unwrap_or_else(|_| panic!("Bad y at line {}", lineno + 1));
        // Round to nearest i32
        let xi = x.round() as i32;
        let yi = y.round() as i32;
        points.push(quote! { embedded_graphics::geometry::Point::new(#xi, #yi) });
    }

    // Second and third arguments (color, stroke width) are arbitrary expressions
    let color_expr = &args[1];
    let width_expr = &args[2];

    let expanded = quote! {
        {
            use embedded_graphics::prelude::*;
            use embedded_graphics::primitives::{Polyline, PrimitiveStyle};

            const POINTS: &[embedded_graphics::geometry::Point] = &[
                #(#points),*
            ];

            Polyline::new(POINTS)
                .into_styled(PrimitiveStyle::with_stroke(#color_expr, #width_expr))
        }
    };

    TokenStream::from(expanded)
}
