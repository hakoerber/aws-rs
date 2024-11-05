use proc_macro::TokenStream;
use quote::quote;

#[derive(Debug)]
enum TransparentKind {
    NewtypeStruct {
        ty: syn::Type,
    },
    SimpleEnum {
        variants: Vec<(syn::Ident, Option<syn::LitStr>)>,
    },
}

#[derive(Debug)]
enum Translator {
    Serde,
    Manual,
    Transparent(TransparentKind),
}

fn parse_enum_attributes(attrs: &mut Vec<syn::Attribute>) -> Option<syn::LitStr> {
    let index_of_tag_attribute = attrs
        .iter()
        .enumerate()
        .filter(|&(_i, attr)| attr.style == syn::AttrStyle::Outer)
        .find_map(|(i, attr)| match attr.meta {
            syn::Meta::List(ref meta_list) => {
                if let (Some(name), 1) = (
                    meta_list.path.segments.first(),
                    meta_list.path.segments.len(),
                ) {
                    if name.ident == "tag" {
                        Some((i, meta_list.clone()))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        });

    match index_of_tag_attribute {
        Some((i, meta_list)) => {
            // `i` came from `enumerate()` and is guaranteed to be in bounds
            let removed_attribute = attrs.remove(i);
            drop(removed_attribute);

            let expr: syn::Expr =
                syn::parse(meta_list.tokens.into()).expect("expected expr in macro attribute");

            match expr {
                syn::Expr::Assign(ref assign) => {
                    match *assign.left {
                        syn::Expr::Path(ref exprpath) => {
                            let segments = &exprpath.path.segments;
                            let (Some(segment), 1) = (segments.first(), segments.len()) else {
                                panic!("invalid attribute key")
                            };

                            assert!(segment.ident == "rename", "invalid attribute key");
                        }
                        _ => panic!("invalid expression in enum variant attribute, left side"),
                    }

                    match *assign.right {
                        syn::Expr::Lit(ref expr_lit) => match expr_lit.lit {
                            syn::Lit::Str(ref lit_str) => Some(lit_str.clone()),
                            _ => panic!("invalid literal in enum variant attribute"),
                        },
                        _ => panic!("invalid expression in enum variant attribute, right side"),
                    }
                }
                _ => panic!("invalid expression in enum variant attribute"),
            }
        }
        None => None,
    }
}

fn parse_transparent_enum(mut e: syn::ItemEnum) -> (Translator, syn::Item) {
    let variants = e
        .variants
        .iter_mut()
        .map(|variant| {
            assert!(
                variant.discriminant.is_none(),
                "variant cannot have an explicit discriminant"
            );
            match variant.fields {
                syn::Fields::Unit => (),
                _ => panic!("enum cannot have fields in variants"),
            }
            let rename = parse_enum_attributes(&mut variant.attrs);

            (variant.ident.clone(), rename)
        })
        .collect::<Vec<(syn::Ident, Option<syn::LitStr>)>>();

    (
        Translator::Transparent(TransparentKind::SimpleEnum { variants }),
        e.into(),
    )
}

pub(crate) fn transform(attr: TokenStream, item: TokenStream) -> TokenStream {
    let root = quote! {::aws_lib};

    let expr: syn::Expr = syn::parse(attr).expect("expected expr in macro attribute");

    let elem = syn::parse_macro_input!(item as syn::Item);

    let syn::Expr::Assign(assign) = expr else {
        panic!("invalid expression in macro attribute")
    };

    match *assign.left {
        syn::Expr::Path(ref exprpath) => {
            let segments = &exprpath.path.segments;
            let (Some(segment), 1) = (segments.first(), segments.len()) else {
                panic!("invalid attribute key")
            };

            assert!(segment.ident == "translate", "invalid attribute key");
        }
        _ => panic!("invalid expression in tag field attribute, left side"),
    }

    let (translator, elem) = match *assign.right {
        syn::Expr::Path(ref exprpath) => {
            let segments = &exprpath.path.segments;
            let (Some(segment), 1) = (segments.first(), segments.len()) else {
                panic!("invalid attribute key")
            };

            match segment.ident.to_string().as_str() {
                "serde" => (Translator::Serde, elem),
                "manual" => (Translator::Manual, elem),
                "transparent" => match elem {
                    syn::Item::Struct(ref s) => match s.fields {
                        syn::Fields::Unnamed(ref fields) => {
                            let (Some(field), 1) = (fields.unnamed.first(), fields.unnamed.len())
                            else {
                                panic!(
                                        "transparent translation is only available for newtype-style macros"
                                    )
                            };
                            (
                                Translator::Transparent(TransparentKind::NewtypeStruct {
                                    ty: field.ty.clone(),
                                }),
                                elem,
                            )
                        }
                        _ => panic!(
                            "transparent translation is only available for newtype-style macros"
                        ),
                    },

                    syn::Item::Enum(e) => parse_transparent_enum(e),
                    _ => {
                        panic!("transparent translation is only available for newtype-style macros")
                    }
                },
                t => panic!("invalid translator {t}"),
            }
        }
        _ => panic!("invalid expression in tag field attribute, left side"),
    };

    let name = match elem {
        syn::Item::Struct(ref s) => &s.ident,
        syn::Item::Enum(ref e) => &e.ident,
        _ => panic!("only applicable to structs/enums"),
    };

    let translator = match translator {
        Translator::Serde => quote! {
            impl #root::tags::TranslatableSerde for #name {}

            impl #root::tags::TagValue<#name> for #name {
                type Error = #root::tags::ParseTagValueError;
                type Translator = #root::tags::TranslateSerde;
            }
        },
        Translator::Manual => quote! {
            impl #root::tags::TranslatableManual for #name {}

            impl #root::tags::TagValue<#name> for #name {
                type Error = #root::tags::ParseTagValueError;
                type Translator = #root::tags::TranslateManual;
            }
        },
        Translator::Transparent(kind) => match kind {
            TransparentKind::NewtypeStruct { ty } => quote! {
                impl #root::tags::TranslatableManual for #name {}

                impl #root::tags::TagValue<#name> for #name {
                    type Error = #root::tags::ParseTagValueError;
                    type Translator = #root::tags::TranslateManual;
                }

                impl TryFrom<#root::tags::RawTagValue> for #name {
                    type Error = #root::tags::ParseTagValueError;

                    fn try_from(value: #root::tags::RawTagValue) -> Result<Self, Self::Error> {
                        Ok(Self(<#ty as #root::tags::TagValue<#ty>>::from_raw_tag(value)?))
                    }
                }

                impl From<#name> for RawTagValue {
                    fn from(value: #name) -> Self {
                        <#ty as #root::tags::TagValue<#ty>>::into_raw_tag(value.0)
                    }
                }
            },

            TransparentKind::SimpleEnum { variants } => {
                let (into_raw_tag_mapping, from_raw_tag_mapping): (Vec<_>, Vec<_>) = variants
                    .into_iter()
                    .map(|(variant, rename)| {
                        let lit = rename
                            .map(|r| r.value())
                            .unwrap_or_else(|| variant.to_string());
                        (
                            quote! {
                                #name::#variant => #root::tags::RawTagValue::new(#lit.to_owned()),
                            },
                            quote! {
                                #lit => Self::#variant,
                            },
                        )
                    })
                    .unzip();

                quote! {
                    impl #root::tags::TranslatableManual for #name {}

                    impl #root::tags::TagValue<#name> for #name {
                        type Error = #root::tags::ParseTagValueError;
                        type Translator = #root::tags::TranslateManual;
                    }

                    impl From<#name> for #root::tags::RawTagValue {
                        fn from(value: #name) -> Self {
                            match value {
                                #(#into_raw_tag_mapping)
                                *
                            }
                        }
                    }

                    impl TryFrom<#root::tags::RawTagValue> for #name {
                        type Error = #root::tags::ParseTagValueError;

                        fn try_from(value: #root::tags::RawTagValue) -> Result<Self, Self::Error> {
                            Ok(match value.as_str() {
                                #(#from_raw_tag_mapping)
                                *
                                _ => return Err(#root::tags::ParseTagValueError::InvalidValue {
                                    value,
                                    message: "invalid enum value".to_owned(),
                                }),
                            })
                        }
                    }
                }
            }
        },
    };

    quote! {
        #elem
        #translator
    }
    .into()
}
