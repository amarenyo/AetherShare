import type React from 'react';

type MdProps = React.DetailedHTMLProps<React.HTMLAttributes<HTMLElement>, HTMLElement> & {
  [key: string]: unknown;
};

interface MaterialWebElements {
  'md-filled-button': MdProps;
  'md-filled-tonal-button': MdProps;
  'md-outlined-button': MdProps;
  'md-text-button': MdProps;
  'md-chip-set': MdProps;
  'md-input-chip': MdProps;
  'md-assist-chip': MdProps;
  'md-divider': MdProps;
  'md-elevation': MdProps;
  'md-icon-button': MdProps;
  'md-filled-tonal-icon-button': MdProps;
  'md-menu': MdProps;
  'md-menu-item': MdProps;
  'md-linear-progress': MdProps;
  'md-ripple': MdProps;
  'md-switch': MdProps;
  'md-outlined-text-field': MdProps;
  'md-filled-card': MdProps;
  'md-outlined-card': MdProps;
  'md-outlined-segmented-button': MdProps;
  'md-outlined-segmented-button-set': MdProps;
}

declare module 'react' {
  namespace JSX {
    interface IntrinsicElements extends MaterialWebElements {}
  }
}

declare module 'react/jsx-runtime' {
  namespace JSX {
    interface IntrinsicElements extends MaterialWebElements {}
  }
}
