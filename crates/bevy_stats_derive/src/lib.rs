//! Derive macros for `bevy_stats`.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, LitStr, parse_macro_input, spanned::Spanned};

/// Derives `bevy_stats::StatsBound`, keeping annotated fields in sync with
/// the stat graph.
///
/// Field attributes:
///
/// - `#[stat("Name")]` — *read* field: refreshed from the resolved stat
///   value whenever the entity's stats change.
/// - `#[stat("Name", write)]` — *two-way* field: the component is
///   authoritative; changes are written into the stat's base value (and the
///   field still reflects the resolved stat, which matters if modifiers or
///   instant effects touch it).
///
/// Fields without a `#[stat]` attribute are left alone. Field types must
/// implement `bevy_stats::StatValue` (`f32`, `f64`, `bool`, and integers do).
///
/// Register the component with
/// `app.register_stats_component::<T>()`; a `Stats` component is added on
/// spawn if missing, `write` fields seed their stats, and all fields are
/// initialized from the graph.
///
/// ```ignore
/// #[derive(Component, StatSync)]
/// struct Health {
///     #[stat("Life.max")]
///     max: f32,
///     #[stat("Life.current", write)]
///     current: f32,
/// }
/// ```
#[proc_macro_derive(StatSync, attributes(stat))]
pub fn derive_stat_sync(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

struct StatField {
    ident: syn::Ident,
    ty: syn::Type,
    stat: String,
    write: bool,
}

fn expand(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            input.span(),
            "`StatSync` can only be derived for structs",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new(
            input.span(),
            "`StatSync` requires named fields",
        ));
    };

    let mut stat_fields = Vec::new();
    for field in &fields.named {
        for attr in &field.attrs {
            if !attr.path().is_ident("stat") {
                continue;
            }
            let mut stat: Option<String> = None;
            let mut write = false;
            attr.parse_args_with(|meta: syn::parse::ParseStream| {
                let lit: LitStr = meta.parse()?;
                stat = Some(lit.value());
                while meta.peek(syn::Token![,]) {
                    meta.parse::<syn::Token![,]>()?;
                    let ident: syn::Ident = meta.parse()?;
                    match ident.to_string().as_str() {
                        "write" => write = true,
                        "read" => {}
                        other => {
                            return Err(syn::Error::new(
                                ident.span(),
                                format!("unknown `#[stat]` option `{other}`; expected `write`"),
                            ));
                        }
                    }
                }
                Ok(())
            })?;
            let stat = stat.ok_or_else(|| {
                syn::Error::new(attr.span(), "expected `#[stat(\"StatName\", ...)]`")
            })?;
            stat_fields.push(StatField {
                ident: field.ident.clone().expect("named field"),
                ty: field.ty.clone(),
                stat,
                write,
            });
        }
    }

    if stat_fields.is_empty() {
        return Err(syn::Error::new(
            input.span(),
            "`StatSync` requires at least one `#[stat(\"Name\")]` field",
        ));
    }

    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let writes = stat_fields.iter().filter(|f| f.write).map(|f| {
        let ident = &f.ident;
        let ty = &f.ty;
        let stat = &f.stat;
        quote! {
            let _ = stats.set(
                entity,
                #stat,
                <#ty as ::bevy_stats::StatValue>::to_stat(&self.#ident),
            );
        }
    });

    let reads = stat_fields.iter().map(|f| {
        let ident = &f.ident;
        let ty = &f.ty;
        let stat = &f.stat;
        quote! {
            let value = <#ty as ::bevy_stats::StatValue>::from_stat(stats.get(entity, #stat));
            if self.#ident != value {
                self.#ident = value;
                changed = true;
            }
        }
    });

    Ok(quote! {
        impl #impl_generics ::bevy_stats::StatsBound for #name #ty_generics #where_clause {
            fn write_stats(
                &self,
                entity: ::bevy_ecs::entity::Entity,
                stats: &mut ::bevy_stats::StatsMutator,
            ) {
                #(#writes)*
            }

            fn read_stats(
                &mut self,
                entity: ::bevy_ecs::entity::Entity,
                stats: &mut ::bevy_stats::StatsMutator,
            ) -> bool {
                let mut changed = false;
                #(#reads)*
                changed
            }
        }
    })
}
