use proc_macro::TokenStream;
use quote::quote;

#[derive(Debug)]
struct Input {
    ident: syn::Ident,
    vis: syn::Visibility,
    elements: Vec<Element>,
}

#[derive(Debug)]
enum ElementKind {
    Required,
    Optional,
}

#[derive(Debug)]
struct Element {
    ident: syn::Ident,
    vis: syn::Visibility,
    ty: syn::Path,
    kind: ElementKind,
    name: String,
    attrs: Vec<syn::Attribute>,
}

fn is_cfg_attribute(attr: &syn::Attribute) -> bool {
    match attr.meta {
        syn::Meta::List(ref meta_list) => meta_list.path.is_ident("cfg"),
        _ => false,
    }
}

fn cfg_attrs(v: &[syn::Attribute]) -> Vec<&syn::Attribute> {
    v.iter()
        .filter(|attr: &&syn::Attribute| is_cfg_attribute(attr))
        .collect()
}

fn parse_type(input: syn::Type) -> (syn::Path, ElementKind) {
    match input {
        syn::Type::Path(ty) => {
            let segments = ty.path.segments.clone();
            let first = segments.first().expect("segments is empty");

            let (ident, optional) = match first.ident.to_string().as_str() {
                "Option" => match first.arguments {
                    // It may not be required to parse this, we could just
                    // extract the inner type and let the compiler beat the
                    // caller up if there is some bullshit happening.
                    syn::PathArguments::AngleBracketed(ref genargs) => {
                        let mut args = genargs.args.clone();
                        match genargs.args.len() {
                            1 => {
                                let ty = args.pop().expect("genargs are empty");
                                let ty = match ty {
                                    syn::punctuated::Pair::Punctuated(node, _punct) => node,
                                    syn::punctuated::Pair::End(node) => node,
                                };
                                match ty {
                                    syn::GenericArgument::Type(ty) => match ty {
                                        syn::Type::Path(ty) => (ty, ElementKind::Optional),
                                        _ => panic!("invalid generic type for Option"),
                                    },

                                    _ => panic!("need simple owned Option generic"),
                                }
                            }
                            _ => panic!("wrong number of Option generic arguments"),
                        }
                    }
                    _ => panic!("invalid Option usage"),
                },
                _ => (ty, ElementKind::Required),
            };

            (ident.path, optional)
        }
        _ => panic!("invalid field type"),
    }
}

fn parse_field_attrs(attrs: &mut Vec<syn::Attribute>) -> Option<String> {
    let index_of_tag_attribute = attrs
        .iter()
        .enumerate()
        .filter(|&(_i, attr)| attr.style == syn::AttrStyle::Outer)
        .find_map(|(i, attr)| match attr.meta {
            syn::Meta::List(ref meta_list) => {
                if meta_list.path.is_ident("tag") {
                    Some((i, meta_list.clone()))
                } else {
                    None
                }
            }
            _ => None,
        });

    match index_of_tag_attribute {
        Some((i, meta_list)) => {
            let removed_attribute = attrs.remove(i);
            drop(removed_attribute);

            let expr: syn::Expr = match meta_list.parse_args() {
                Ok(expr) => expr,
                Err(e) => panic!("failed parsing tag field attribute: {e}"),
            };

            let syn::Expr::Assign(assign) = expr else {
                panic!("invalid expression in tag field attribute")
            };

            match *assign.left {
                syn::Expr::Path(ref exprpath) => {
                    assert!(
                        exprpath.path.is_ident("key"),
                        "invalid tag field attribute key"
                    );
                }
                _ => panic!("invalid expression in tag field attribute, left side"),
            }

            match *assign.right {
                syn::Expr::Lit(ref expr_lit) => match expr_lit.lit {
                    syn::Lit::Str(ref lit_str) => Some(lit_str.value()),
                    _ => panic!("right side of tag field not a string literal"),
                },
                _ => panic!("right side of tag field attribute not a literal"),
            }
        }
        None => None,
    }
}

fn parse_fields(input: impl IntoIterator<Item = syn::Field>) -> Vec<Element> {
    let mut elements = Vec::new();
    for mut field in input {
        let ident = field.ident.expect("tuple structs not supported");
        let vis = field.vis;
        let (ty, kind) = parse_type(field.ty);

        let name = parse_field_attrs(&mut field.attrs);

        elements.push(Element {
            ident: ident.clone(),
            vis,
            ty,
            kind,
            name: name.unwrap_or_else(|| ident.to_string()),
            attrs: field.attrs,
        });
    }
    elements
}

fn parse_struct(input: syn::ItemStruct) -> Input {
    Input {
        ident: input.ident,
        vis: input.vis,
        elements: match input.fields {
            syn::Fields::Named(fields) => parse_fields(fields.named),
            _ => panic!("invalid fields"),
        },
    }
}

fn build_output(input: Input) -> TokenStream {
    let root = quote! { ::aws_lib };

    let ident = input.ident;
    let vis = input.vis;

    let type_definition = {
        let elements: Vec<proc_macro2::TokenStream> = input
            .elements
            .iter()
            .map(|element| {
                let ident = &element.ident;
                let vis = &element.vis;
                let ty = &element.ty;
                let attrs = &element.attrs;
                match element.kind {
                    ElementKind::Required => {
                        quote!(
                            #(#attrs)
                            *
                            #vis #ident: #ty
                        )
                    }
                    ElementKind::Optional => {
                        quote!(
                            #(#attrs)
                            *
                            #vis #ident: ::std::option::Option<#ty>
                        )
                    }
                }
            })
            .collect();

        quote! {
            #vis struct #ident {
                #(#elements),*
            }
        }
    };

    let impls = {
        let params = input.elements.iter().map(|element| {
            let ident = &element.ident;
            let ty = &element.ty;
            let attrs = cfg_attrs(&element.attrs);
            match element.kind {
                ElementKind::Required => quote! {
                    #(#attrs)
                    *
                    #ident: #ty
                },
                ElementKind::Optional => quote! {
                    #(#attrs)
                    *
                    #ident: ::std::option::Option<#ty>
                },
            }
        });

        let from_fields: Vec<proc_macro2::TokenStream> = input
            .elements
            .iter()
            .map(|element| {
                let ident = &element.ident;
                let attrs = cfg_attrs(&element.attrs);
                quote! {
                    #(#attrs)
                    *
                    #ident: #ident
                }
            })
            .collect();

        let from_tags_fields: Vec<proc_macro2::TokenStream> = input.elements.iter().map(|element| {
            let ident = &element.ident;
            let ty = &element.ty;
            let tag_name = &element.name;
            let attrs = cfg_attrs(&element.attrs);

            let try_convert = quote! {
                let value: ::std::result::Result<#ty, #root::tags::ParseTagsError> = <#ty as #root::tags::TagValue<#ty>>::from_raw_tag(value)
                    .map_err(
                        |e| #root::tags::ParseTagsError::ParseTag(#root::tags::ParseTagError::InvalidTagValue {
                            key,
                            inner: <<#ty as #root::tags::TagValue<#ty>>::Error as Into<#root::tags::ParseTagValueError>>::into(e),
                        }
                    )
                );

                let value = match value {
                    ::std::result::Result::Ok(v) => v,
                    ::std::result::Result::Err(e) => {
                        return Err(e);
                    }
                };

                value
            };

            let transformer = match element.kind {
                ElementKind::Required => {
                    quote! {
                        let value: #root::tags::RawTagValue = value.ok_or_else(|| #root::tags::ParseTagsError::TagNotFound {
                                key: key.clone()
                            })?
                            .clone();

                        let value = {
                             #try_convert
                        };

                        value

                    }
                }
                ElementKind::Optional => {
                    quote! {
                        let value: ::std::option::Option<#ty> = value.map(|value: #root::tags::RawTagValue| {
                            let value = {
                                 #try_convert
                            };
                            Ok(value)
                        }).transpose()?;
                        value
                    }
                }
            };

            quote! {
                #(#attrs)
                *
                #ident: {
                    let key: #root::tags::TagKey = #root::tags::TagKey::new(#tag_name.to_owned());

                    let value: ::std::option::Option<#root::tags::RawTagValue> = tags
                        .as_slice()
                        .iter()
                        .find(|tag| tag.key() == #tag_name)
                        .map(|tag| tag.value()).cloned();

                    let value = {
                         #transformer
                    };

                    value
                }
            }
        }).collect();

        let fields_to_tags: Vec<proc_macro2::TokenStream> = input
            .elements
            .iter()
            .map(|element| {
                let ident = &element.ident;
                let ty= &element.ty;
                let tag_name = &element.name;
                let attrs= &element.attrs;
                match element.kind {
                    ElementKind::Required => {
                        quote! {
                            #(#attrs)
                            *
                            {
                                let key = #root::tags::TagKey::new(#tag_name.to_owned());
                                let value: #root::tags::RawTagValue = <#ty as #root::tags::TagValue<#ty>>::into_raw_tag(self.#ident);
                                v.push(#root::tags::RawTag::new(key, value));
                            }
                        }
                    }
                    ElementKind::Optional => {
                        quote! {
                            #(#attrs)
                            *
                            {
                                match self.#ident {
                                    ::std::option::Option::Some(value) => {
                                        let key = #root::tags::TagKey::new(#tag_name.to_owned());
                                        let value: #root::tags::RawTagValue = <#ty as #root::tags::TagValue<#ty>>::into_raw_tag(value);
                                        v.push(#root::tags::RawTag::new(key, value));
                                    },
                                    ::std::option::Option::None => {
                                        // do not serialize none values
                                    },
                                }
                            }
                        }
                    }
                }
            })
            .collect();

        quote! {
            impl #ident {
                #vis fn from_values(#(#params),*) -> Self {
                    Self {
                        #(#from_fields),*
                    }
                }

                #vis fn from_tags(tags: #root::tags::TagList) -> Result<Self, #root::tags::ParseTagsError> {
                    Ok(Self {
                        #(#from_tags_fields),*
                    })
                }

                #vis fn into_tags(self) -> #root::tags::TagList {
                    let mut v = ::std::vec::Vec::new();
                    {
                        #(#fields_to_tags);*;
                    }
                    #root::tags::TagList::from_vec(v)
                }
            }
        }
    };

    quote! {
        #type_definition
        #impls
    }
    .into()
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "this is the usual signature for proc macros, and the inner function should have the same signature"
)]
pub(crate) fn transform(attr: TokenStream, item: TokenStream) -> TokenStream {
    assert!(
        attr.is_empty(),
        "cannot take any attribute macro attributes"
    );

    let input = syn::parse_macro_input!(item as syn::Item);

    let input = match input {
        syn::Item::Struct(s) => parse_struct(s),
        _ => panic!("only applicable to structs"),
    };

    build_output(input)
}
